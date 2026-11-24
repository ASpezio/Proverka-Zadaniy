//! ZIP-контейнеры (docx, odt, xlsx): чтение (stored + deflate) и запись (stored).
//! Включает собственный декодер DEFLATE (RFC 1951) — без внешних библиотек.

// ------------------------------------------------------------------ INFLATE

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit: 0, nbits: 0 }
    }
    #[inline]
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.nbits < n {
            if self.pos >= self.data.len() {
                // допускаем «хвост» из нулей — некоторые архиваторы обрезают
                if self.nbits + 32 < n {
                    return Err("deflate: неожиданный конец данных".into());
                }
                self.nbits += 8;
                continue;
            }
            self.bit |= (self.data[self.pos] as u32) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        Ok(())
    }
    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align(&mut self) {
        let r = self.nbits % 8;
        self.bit >>= r;
        self.nbits -= r;
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }
    fn decode(&self, br: &mut BitReader) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                let i = (index + (code - first)) as usize;
                return self.symbols.get(i).copied().ok_or_else(|| "deflate: плохой код".to_string());
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("deflate: код не найден".into())
    }
}

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

pub fn inflate(data: &[u8], size_hint: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.max(1024));
    let mut br = BitReader::new(data);
    loop {
        let last = br.bits(1)?;
        let typ = br.bits(2)?;
        match typ {
            0 => {
                br.align();
                let len = br.bits(16)? as usize;
                let _nlen = br.bits(16)?;
                // после выравнивания буфер битов пуст, читаем байты напрямую
                for _ in 0..len {
                    out.push(br.bits(8)? as u8);
                }
            }
            1 => {
                let mut l = [0u8; 288];
                for (i, x) in l.iter_mut().enumerate() {
                    *x = if i < 144 { 8 } else if i < 256 { 9 } else if i < 280 { 7 } else { 8 };
                }
                let lit = Huffman::new(&l)?;
                let dist = Huffman::new(&[5u8; 30])?;
                codes(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let hlit = br.bits(5)? as usize + 257;
                let hdist = br.bits(5)? as usize + 1;
                let hclen = br.bits(4)? as usize + 4;
                const ORD: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORD.iter().take(hclen) {
                    cl[o] = br.bits(3)? as u8;
                }
                let clh = Huffman::new(&cl)?;
                let mut lens = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = clh.decode(&mut br)?;
                    match sym {
                        0..=15 => {
                            lens[i] = sym as u8;
                            i += 1;
                        }
                        16 => {
                            if i == 0 {
                                return Err("deflate: повтор без предыдущего".into());
                            }
                            let prev = lens[i - 1];
                            let r = 3 + br.bits(2)? as usize;
                            for _ in 0..r {
                                if i < lens.len() {
                                    lens[i] = prev;
                                    i += 1;
                                }
                            }
                        }
                        17 => {
                            let r = 3 + br.bits(3)? as usize;
                            i += r;
                        }
                        _ => {
                            let r = 11 + br.bits(7)? as usize;
                            i += r;
                        }
                    }
                }
                lens.truncate(hlit + hdist);
                let lit = Huffman::new(&lens[..hlit])?;
                let dist = Huffman::new(&lens[hlit..])?;
                codes(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err("deflate: неверный тип блока".into()),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

fn codes(br: &mut BitReader, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Result<(), String> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = (sym - 257) as usize;
            if s >= 29 {
                return Err("deflate: плохая длина".into());
            }
            let len = LBASE[s] as usize + br.bits(LEXT[s] as u32)? as usize;
            let ds = dist.decode(br)? as usize;
            if ds >= 30 {
                return Err("deflate: плохое расстояние".into());
            }
            let d = DBASE[ds] as usize + br.bits(DEXT[ds] as u32)? as usize;
            if d > out.len() {
                return Err("deflate: расстояние за пределами".into());
            }
            let st = out.len() - d;
            for k in 0..len {
                let b = out[st + k];
                out.push(b);
            }
        }
    }
}

// ------------------------------------------------------------------ ЧТЕНИЕ ZIP

fn u16le(b: &[u8], o: usize) -> usize {
    if o + 2 > b.len() {
        return 0;
    }
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}
fn u32le(b: &[u8], o: usize) -> usize {
    if o + 4 > b.len() {
        return 0;
    }
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize
}

pub struct ZipEntry {
    pub name: String,
    method: u16,
    csize: usize,
    usize_: usize,
    local_off: usize,
    /// время изменения файла в архиве (секунды от 1970, по часам архиватора)
    pub mtime: u64,
}

/// Имя файла в архиве. Флаг 11 — UTF-8; поле 0x7075 — имя в UTF-8 от Info-ZIP;
/// иначе архив создан «Отправить → Сжатая ZIP-папка» Windows или старым
/// архиватором, и имена записаны в кодировке DOS (CP866) — без этого русские
/// имена превращаются в «����».
pub fn decode_name(raw: &[u8], flags: usize, extra: &[u8]) -> String {
    if flags & 0x800 != 0 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut i = 0;
    while i + 4 <= extra.len() {
        let id = u16le(extra, i);
        let sz = u16le(extra, i + 2);
        if id == 0x7075 && sz >= 5 && i + 4 + sz <= extra.len() {
            if let Ok(n) = std::str::from_utf8(&extra[i + 9..i + 4 + sz]) {
                return n.to_string();
            }
        }
        i += 4 + sz;
    }
    if let Ok(n) = std::str::from_utf8(raw) {
        // байты CP866 иногда случайно складываются в «правильный» UTF-8 из
        // экзотических письменностей — тогда это всё-таки CP866
        let odd = n.chars().any(|c| (c as u32) > 0x04FF && !"–—«»№“”„’…".contains(c));
        if !odd {
            return n.to_string();
        }
    }
    raw.iter().map(|&b| cp866(b)).collect()
}

fn cp866(b: u8) -> char {
    match b {
        0x00..=0x7F => b as char,
        0x80..=0xAF => char::from_u32(0x0410 + (b as u32 - 0x80)).unwrap_or('_'),
        0xE0..=0xEF => char::from_u32(0x0440 + (b as u32 - 0xE0)).unwrap_or('_'),
        0xF0 => 'Ё',
        0xF1 => 'ё',
        0xF2 => 'Є',
        0xF3 => 'є',
        0xF4 => 'Ї',
        0xF5 => 'ї',
        0xF6 => 'Ў',
        0xF7 => 'ў',
        0xFC => '№',
        0xFF => ' ',
        _ => '_',
    }
}

/// Время и дата MS-DOS → секунды от 1970 (0, если не заданы).
fn dos_time(time: usize, date: usize) -> u64 {
    if date == 0 {
        return 0;
    }
    let (y, m, d) = (1980 + (date >> 9) as i64, ((date >> 5) & 15) as i64, (date & 31) as i64);
    if !(1..=12).contains(&m) || d == 0 {
        return 0;
    }
    // дни от 1970-01-01 (алгоритм Г. Хиннанта)
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = ((time >> 11) * 3600 + ((time >> 5) & 63) * 60 + (time & 31) * 2) as i64;
    (days * 86400 + secs).max(0) as u64
}

pub struct ZipArchive<'a> {
    data: &'a [u8],
    pub entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {
    pub fn open(data: &'a [u8]) -> Result<ZipArchive<'a>, String> {
        if data.len() < 22 || &data[0..2] != b"PK" {
            return Err("файл не является ZIP-архивом (docx/odt)".into());
        }
        // ищем End Of Central Directory
        let mut eocd = None;
        let lo = data.len().saturating_sub(22 + 65535);
        let mut i = data.len() - 22;
        loop {
            if &data[i..i + 4] == b"PK\x05\x06" {
                eocd = Some(i);
                break;
            }
            if i == lo {
                break;
            }
            i -= 1;
        }
        let mut entries = Vec::new();
        if let Some(e) = eocd {
            let count = u16le(data, e + 10);
            let mut p = u32le(data, e + 16);
            for _ in 0..count {
                if p + 46 > data.len() || &data[p..p + 4] != b"PK\x01\x02" {
                    break;
                }
                let method = u16le(data, p + 10) as u16;
                let csize = u32le(data, p + 20);
                let usz = u32le(data, p + 24);
                let nlen = u16le(data, p + 28);
                let elen = u16le(data, p + 30);
                let clen = u16le(data, p + 32);
                let off = u32le(data, p + 42);
                let flags = u16le(data, p + 8);
                let raw = &data[p + 46..(p + 46 + nlen).min(data.len())];
                let extra = &data[(p + 46 + nlen).min(data.len())..(p + 46 + nlen + elen).min(data.len())];
                let name = decode_name(raw, flags, extra);
                let mtime = dos_time(u16le(data, p + 12), u16le(data, p + 14));
                entries.push(ZipEntry { name, method, csize, usize_: usz, local_off: off, mtime });
                p += 46 + nlen + elen + clen;
            }
        }
        if entries.is_empty() {
            // запасной путь: сканируем локальные заголовки
            let mut p = 0;
            while p + 30 <= data.len() {
                if &data[p..p + 4] != b"PK\x03\x04" {
                    break;
                }
                let method = u16le(data, p + 8) as u16;
                let csize = u32le(data, p + 18);
                let usz = u32le(data, p + 22);
                let nlen = u16le(data, p + 26);
                let elen = u16le(data, p + 28);
                let flags = u16le(data, p + 6);
                let raw = &data[p + 30..(p + 30 + nlen).min(data.len())];
                let extra = &data[(p + 30 + nlen).min(data.len())..(p + 30 + nlen + elen).min(data.len())];
                let name = decode_name(raw, flags, extra);
                let mtime = dos_time(u16le(data, p + 10), u16le(data, p + 12));
                entries.push(ZipEntry { name, method, csize, usize_: usz, local_off: p, mtime });
                p += 30 + nlen + elen + csize;
            }
        }
        Ok(ZipArchive { data, entries })
    }

    pub fn mtime(&self, name: &str) -> u64 {
        self.entries.iter().find(|e| e.name == name).map(|e| e.mtime).unwrap_or(0)
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.entries.iter().find(|e| e.name == name || e.name.eq_ignore_ascii_case(name))?;
        let d = self.data;
        let p = e.local_off;
        if p + 30 > d.len() || &d[p..p + 4] != b"PK\x03\x04" {
            return None;
        }
        let nlen = u16le(d, p + 26);
        let elen = u16le(d, p + 28);
        let st = p + 30 + nlen + elen;
        let end = (st + e.csize).min(d.len());
        let raw = &d[st..end];
        match e.method {
            0 => Some(raw.to_vec()),
            8 => inflate(raw, e.usize_).ok(),
            _ => None,
        }
    }

    pub fn read_string(&self, name: &str) -> Option<String> {
        self.read(name).map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

// ------------------------------------------------------------------ ЗАПИСЬ ZIP

const fn make_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC_TABLE: [u32; 256] = make_crc_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFFFFFFu32;
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

pub struct ZipWriter {
    buf: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl ZipWriter {
    pub fn new() -> Self {
        ZipWriter { buf: Vec::new(), central: Vec::new(), count: 0 }
    }
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let crc = crc32(data);
        let off = self.buf.len() as u32;
        let nb = name.as_bytes();
        let size = data.len() as u32;
        // локальный заголовок; флаг 0x0800 = имена в UTF-8
        self.buf.extend_from_slice(b"PK\x03\x04");
        self.buf.extend_from_slice(&20u16.to_le_bytes());
        self.buf.extend_from_slice(&0x0800u16.to_le_bytes());
        self.buf.extend_from_slice(&0u16.to_le_bytes()); // stored
        self.buf.extend_from_slice(&0u16.to_le_bytes()); // time
        self.buf.extend_from_slice(&0x5A21u16.to_le_bytes()); // date (2025-01-01)
        self.buf.extend_from_slice(&crc.to_le_bytes());
        self.buf.extend_from_slice(&size.to_le_bytes());
        self.buf.extend_from_slice(&size.to_le_bytes());
        self.buf.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        self.buf.extend_from_slice(&0u16.to_le_bytes());
        self.buf.extend_from_slice(nb);
        self.buf.extend_from_slice(data);
        // центральный каталог
        let c = &mut self.central;
        c.extend_from_slice(b"PK\x01\x02");
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&0x0800u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0x5A21u16.to_le_bytes());
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&size.to_le_bytes());
        c.extend_from_slice(&size.to_le_bytes());
        c.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&off.to_le_bytes());
        c.extend_from_slice(nb);
        self.count += 1;
    }
    pub fn add_str(&mut self, name: &str, s: &str) {
        self.add(name, s.as_bytes());
    }
    pub fn finish(mut self) -> Vec<u8> {
        let cd_off = self.buf.len() as u32;
        let cd_len = self.central.len() as u32;
        self.buf.extend_from_slice(&self.central);
        self.buf.extend_from_slice(b"PK\x05\x06");
        self.buf.extend_from_slice(&0u16.to_le_bytes());
        self.buf.extend_from_slice(&0u16.to_le_bytes());
        self.buf.extend_from_slice(&self.count.to_le_bytes());
        self.buf.extend_from_slice(&self.count.to_le_bytes());
        self.buf.extend_from_slice(&cd_len.to_le_bytes());
        self.buf.extend_from_slice(&cd_off.to_le_bytes());
        self.buf.extend_from_slice(&0u16.to_le_bytes());
        self.buf
    }
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn cp866_names() {
        // «Анна Леденцова» в CP866
        let raw: Vec<u8> = vec![0x80, 0xAD, 0xAD, 0xA0, 0x20, 0x8B, 0xA5, 0xA4, 0xA5, 0xAD, 0xE6, 0xAE, 0xA2, 0xA0];
        assert_eq!(super::decode_name(&raw, 0, &[]), "Анна Леденцова");
        assert_eq!(super::decode_name("Задание.xlsx".as_bytes(), 0x800, &[]), "Задание.xlsx");
    }
    #[test]
    fn dos() {
        // 2024-03-15 10:20:30
        assert_eq!(super::dos_time((10 << 11) | (20 << 5) | 15, (44 << 9) | (3 << 5) | 15), 1710498030);
    }
}
