# Сборка и запуск «Проверки заданий» (Windows, PowerShell 5.1+)
# Использование:  powershell -ExecutionPolicy Bypass -File tools\build.ps1 [-Run] [-Force]
param([switch]$Run, [switch]$Force, [switch]$NoPause, [switch]$Notify)
$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$exeName = "Проверка заданий.exe"
$exe = Join-Path $root $exeName
$built = Join-Path $root "src-tauri\target\release\ProverkaZadaniy.exe"
$log = Join-Path $root "build.log"

function Say($t, $c = "Cyan") { Write-Host $t -ForegroundColor $c }

# Нужна ли пересборка: исходники новее, чем .exe?
function NeedsBuild {
    if ($Force) { return $true }
    if (-not (Test-Path $exe)) { return $true }
    $exeTime = (Get-Item $exe).LastWriteTime
    $src = @(Get-ChildItem -Recurse -File (Join-Path $root "src-tauri\src"), (Join-Path $root "ui"), (Join-Path $root "src-tauri\icons") -ErrorAction SilentlyContinue)
    $src += Get-Item (Join-Path $root "src-tauri\Cargo.toml"), (Join-Path $root "src-tauri\tauri.conf.json") -ErrorAction SilentlyContinue
    $newest = ($src | Sort-Object LastWriteTime -Descending | Select-Object -First 1).LastWriteTime
    return ($newest -gt $exeTime)
}

if (-not (NeedsBuild)) {
    if ($Run) { Start-Process $exe }
    exit 0
}

Say "=== Проверка заданий: сборка ==="
# 1. Rust
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $cand = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path $cand) { $cargo = $cand; $env:Path = (Split-Path $cand) + ";" + $env:Path }
}
if (-not $cargo) {
    Say "Rust не найден. Пробую установить через winget (нужен интернет)..." Yellow
    winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
    $cand = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path $cand) { $cargo = $cand; $env:Path = (Split-Path $cand) + ";" + $env:Path }
    if (-not $cargo) {
        Say "Не удалось установить Rust автоматически. Скачайте rustup-init.exe с https://rustup.rs, установите и запустите этот файл снова." Red
        if (-not $NoPause) { Read-Host "Нажмите Enter" }
        exit 1
    }
}
Say ("Rust: " + (& $cargo --version))

# 2. Компилятор C++ (MSVC)
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$hasVC = $false
if (Test-Path $vswhere) {
    $vc = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($vc) { $hasVC = $true }
}
if (-not $hasVC) {
    Say "Не найден компилятор C++ (Visual Studio Build Tools). Устанавливаю через winget..." Yellow
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
}

# 3. Номер и дата сборки — их показывает приложение (слева под названием и в «Настройках»)
$numFile = Join-Path $PSScriptRoot "build_number.txt"
$num = 0
if (Test-Path $numFile) { [int]::TryParse((Get-Content $numFile -Raw).Trim(), [ref]$num) | Out-Null }
$num = $num + 1
$stamp = Get-Date -Format "dd.MM.yyyy HH:mm"
$rev = ""
$git = Get-Command git -ErrorAction SilentlyContinue
if ($git -and (Test-Path (Join-Path $root ".git"))) { $rev = (& git -C $root describe --tags --always --dirty 2>$null) }
if (-not $rev) {
    # без git — короткий отпечаток исходников: видно, из каких файлов собрано
    $sha = [System.Security.Cryptography.SHA1]::Create()
    $files = Get-ChildItem -Recurse -File (Join-Path $root "src-tauri\src"), (Join-Path $root "ui") -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notmatch "\\vendor\\" } | Sort-Object FullName
    $acc = New-Object System.Text.StringBuilder
    foreach ($f in $files) { [void]$acc.Append((Get-FileHash -Algorithm SHA1 $f.FullName).Hash) }
    $rev = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::ASCII.GetBytes($acc.ToString()))) -replace "-", "").Substring(0, 7).ToLower()
}
$version = "v1.$num.0"
$info = "$version · сборка $num · $rev · $stamp"
[IO.File]::WriteAllText((Join-Path $root "src-tauri\build_info.txt"), $info, (New-Object System.Text.UTF8Encoding $false))
Say "Версия: $info"

# 4. Сборка
Say "Собираю приложение (первый раз 3–7 минут, дальше быстрее)..."
$env:CARGO_TERM_COLOR = "never"
"=== $(Get-Date) ===" | Out-File -FilePath $log -Encoding utf8
Push-Location (Join-Path $root "src-tauri")
& $cargo build --release 2>&1 | ForEach-Object {
    $line = "$_"
    Add-Content -Path $log -Value $line -Encoding UTF8
    if ($line -match "^(error|warning: unused)") { Write-Host $line -ForegroundColor Red }
    elseif ($line -match "Compiling|Finished|Downloaded") { Write-Host $line -ForegroundColor DarkGray }
}
$code = $LASTEXITCODE
Pop-Location
"=== exit $code ===" | Add-Content -Path $log -Encoding UTF8

if ($code -ne 0 -or -not (Test-Path $built)) {
    Say "Сборка не удалась. Подробности в файле build.log" Red
    if ($Notify) {
        Add-Type -AssemblyName System.Windows.Forms
        [void][System.Windows.Forms.MessageBox]::Show("Сборка не удалась. Подробности — в файле build.log рядом с приложением.", "Проверка заданий — сборка", "OK", "Error")
    }
    if (-not $NoPause) { Read-Host "Нажмите Enter" }
    exit 1
}
# приложение открыто (или свёрнуто в трей) — файл занят, его не заменить
$procName = [IO.Path]::GetFileNameWithoutExtension($exeName)
$running = Get-Process -Name $procName -ErrorAction SilentlyContinue
if ($running) {
    $close = $true
    if ($Notify) {
        Add-Type -AssemblyName System.Windows.Forms
        $close = ([System.Windows.Forms.MessageBox]::Show("Приложение «Проверка заданий» сейчас открыто (возможно, в трее). Закрыть его, чтобы установить новую версию?", "Проверка заданий — сборка", "YesNo", "Question") -eq "Yes")
    }
    if ($close) { $running | Stop-Process -Force; Start-Sleep -Milliseconds 800 }
}
try {
    Copy-Item $built $exe -Force -ErrorAction Stop
} catch {
    Say "Не удалось заменить $exeName — закройте приложение и запустите сборку снова." Red
    if ($Notify) { [void][System.Windows.Forms.MessageBox]::Show("Не удалось заменить файл приложения: оно открыто. Закройте его (и в трее) и запустите сборку снова.", "Проверка заданий — сборка", "OK", "Warning") }
    if (-not $NoPause) { Read-Host "Нажмите Enter" }
    exit 1
}
Set-Content -Path $numFile -Value $num -Encoding ASCII
Write-Host ""
Write-Host "  ============================================" -ForegroundColor Green
Write-Host "    СБОРКА ГОТОВА: $info" -ForegroundColor Green
Write-Host "  ============================================" -ForegroundColor Green
Write-Host ""
"=== готово: $info ===" | Add-Content -Path $log -Encoding UTF8
# установленная копия (ярлык на рабочем столе) обновляется сразу
$installed = Join-Path $env:LOCALAPPDATA "Programs\Проверка заданий\Проверка заданий.exe"
$isInstalled = Test-Path $installed
if ($isInstalled) {
    & (Join-Path $PSScriptRoot "install.ps1") -Source $root -Quiet
    Say "Установленное приложение обновлено: $installed" Green
}
if ($Notify) {
    [Console]::Beep(880, 150); [Console]::Beep(1175, 220)
    Add-Type -AssemblyName System.Windows.Forms
    if ($isInstalled) {
        $ans = [System.Windows.Forms.MessageBox]::Show("Сборка готова, установленное приложение обновлено.`n`n$info`n`nЗапустить приложение?", "Проверка заданий — сборка готова", "YesNo", "Information")
        if ($ans -eq "Yes") { Start-Process $installed }
    } else {
        $ans = [System.Windows.Forms.MessageBox]::Show("Сборка готова.`n`n$info`n`nУстановить приложение с ярлыком на рабочем столе?`n`nДа — установить и запустить, Нет — просто запустить, Отмена — ничего не делать.", "Проверка заданий — сборка готова", "YesNoCancel", "Information")
        if ($ans -eq "Yes") { & (Join-Path $PSScriptRoot "install.ps1") -Source $root -Quiet; Start-Process $installed }
        elseif ($ans -eq "No") { Start-Process $exe }
    }
} elseif ($Run) { Start-Process $exe }
