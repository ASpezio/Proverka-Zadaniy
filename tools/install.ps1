# Установка «Проверки заданий»: программа в %LOCALAPPDATA%\Programs, ярлыки на рабочем столе
# и в меню «Пуск», запись в «Параметры → Приложения» (оттуда же удаление).
# Использование:
#   Установить.bat                       — установить или обновить из папки проекта
#   install.ps1 -Update -Quiet           — тихое обновление (его запускает само приложение)
#   install.ps1 -Uninstall               — удалить (так вызывает Windows)
param([string]$Source = "", [switch]$Quiet, [switch]$Update, [switch]$Uninstall, [switch]$FromSetup)
$ErrorActionPreference = "Stop"
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}
Add-Type -AssemblyName System.Windows.Forms
$app = "Проверка заданий"
$dir = Join-Path $env:LOCALAPPDATA "Programs\$app"
$exeDst = Join-Path $dir "$app.exe"
$regKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\ProverkaZadaniy"
$deskLnk = Join-Path ([Environment]::GetFolderPath("Desktop")) "$app.lnk"
$startLnk = Join-Path ([Environment]::GetFolderPath("Programs")) "$app.lnk"

function Msg($text, $icon = "Information", $buttons = "OK") {
    if ($Quiet) { return "Yes" }
    return [System.Windows.Forms.MessageBox]::Show($text, $app, $buttons, $icon)
}

# закрыть приложение (и из трея), чтобы заменить файл
function Stop-App {
    $p = Get-Process -Name $app -ErrorAction SilentlyContinue
    if (-not $p) { return }
    $p | ForEach-Object { [void]$_.CloseMainWindow() }
    Start-Sleep -Milliseconds 1200
    Get-Process -Name $app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
}

if ($Uninstall) {
    if ((Msg "Удалить «$app» с компьютера?`n`nОценки, журнал и настройки (папка %APPDATA%\proverka) останутся." "Question" "YesNo") -ne "Yes") { exit 0 }
    Stop-App
    Remove-Item $deskLnk, $startLnk -Force -ErrorAction SilentlyContinue
    Remove-Item $regKey -Recurse -Force -ErrorAction SilentlyContinue
    # скрипт лежит в папке программы — удаляем её чуть позже, когда он закончит
    Start-Process -WindowStyle Hidden cmd.exe -ArgumentList "/c timeout /t 2 /nobreak >nul & rmdir /s /q `"$dir`""
    Msg "«$app» удалена с компьютера." | Out-Null
    exit 0
}

# откуда ставить: папка проекта (tools\..) или распакованный установщик
if ($FromSetup) { $Source = $PSScriptRoot }
if (-not $Source) { $Source = Split-Path -Parent $PSScriptRoot }
$srcExe = @((Join-Path $Source "$app.exe"), (Join-Path $Source "ProverkaZadaniy.exe"), (Join-Path $Source "src-tauri\target\release\ProverkaZadaniy.exe")) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $srcExe) {
    Msg "Не найден файл приложения. Сначала соберите его: «Собрать.bat»." "Error" | Out-Null
    exit 1
}

# версия сборки
$info = ""
$num = 0
foreach ($f in @((Join-Path $Source "src-tauri\build_info.txt"), (Join-Path $Source "build_info.txt"))) {
    if (Test-Path $f) { $info = (Get-Content $f -Raw -Encoding UTF8).Trim(); break }
}
foreach ($f in @((Join-Path $Source "tools\build_number.txt"), (Join-Path $Source "build_number.txt"))) {
    if (Test-Path $f) { [int]::TryParse((Get-Content $f -Raw).Trim(), [ref]$num) | Out-Null; break }
}
$version = if ($num -gt 0) { "1.$num.0" } else { "1.0.0" }

try {
    Stop-App
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item $srcExe $exeDst -Force
    foreach ($doc in @("ИНСТРУКЦИЯ.md", "README.md")) {
        $p = Join-Path $Source $doc
        if (Test-Path $p) { Copy-Item $p (Join-Path $dir "ИНСТРУКЦИЯ.md") -Force; break }
    }
    Copy-Item $PSCommandPath (Join-Path $dir "uninstall.ps1") -Force

    # откуда приложение узнаёт о новых сборках
    $meta = [ordered]@{ source = $(if ($FromSetup) { "" } else { $Source }); build = $num; info = $info; installed = (Get-Date -Format "dd.MM.yyyy HH:mm") }
    [IO.File]::WriteAllText((Join-Path $dir "install.json"), ($meta | ConvertTo-Json), (New-Object System.Text.UTF8Encoding $false))

    # ярлыки
    $sh = New-Object -ComObject WScript.Shell
    foreach ($l in @($deskLnk, $startLnk)) {
        $s = $sh.CreateShortcut($l)
        $s.TargetPath = $exeDst
        $s.WorkingDirectory = $dir
        $s.IconLocation = "$exeDst,0"
        $s.Description = "Проверка студенческих работ нейросетью"
        $s.Save()
    }

    # «Параметры → Приложения»
    New-Item -Path $regKey -Force | Out-Null
    $str = [ordered]@{ DisplayName = $app; DisplayVersion = $version; Publisher = "Spezio"; InstallLocation = $dir; DisplayIcon = "$exeDst,0"; URLInfoAbout = "https://t.me/ASpezio"; UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$dir\uninstall.ps1`" -Uninstall" }
    foreach ($k in $str.Keys) { New-ItemProperty -Path $regKey -Name $k -Value $str[$k] -PropertyType String -Force | Out-Null }
    foreach ($k in @("NoModify", "NoRepair")) { New-ItemProperty -Path $regKey -Name $k -Value 1 -PropertyType DWord -Force | Out-Null }
    New-ItemProperty -Path $regKey -Name EstimatedSize -Value ([int]((Get-Item $exeDst).Length / 1KB)) -PropertyType DWord -Force | Out-Null
} catch {
    Msg "Не удалось установить: $($_.Exception.Message)" "Error" | Out-Null
    exit 1
}

if ($Update) { Start-Process $exeDst; exit 0 }
$ans = Msg "«$app» установлена$(if ($info) { " ($info)" }).`n`nЯрлык — на рабочем столе и в меню «Пуск». Удалить можно в «Параметры → Приложения».`n`nЗапустить сейчас?" "Information" "YesNo"
if ($ans -eq "Yes") { Start-Process $exeDst }
