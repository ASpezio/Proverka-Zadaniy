# Один файл-установщик для другого компьютера: «dist\Установка — Проверка заданий.exe».
# Собирается встроенной в Windows программой IExpress из уже собранного приложения.
param([switch]$NoPause)
$ErrorActionPreference = "Stop"
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "Проверка заданий.exe"
if (-not (Test-Path $exe)) { Write-Host "Сначала соберите приложение: Собрать.bat" -ForegroundColor Red; if (-not $NoPause) { Read-Host "Нажмите Enter" }; exit 1 }
$stage = Join-Path $env:TEMP "proverka-setup"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
# внутри установщика — только латинские имена: IExpress плохо дружит с другими
Copy-Item $exe (Join-Path $stage "ProverkaZadaniy.exe")
Copy-Item (Join-Path $PSScriptRoot "install.ps1") (Join-Path $stage "install.ps1")
Copy-Item (Join-Path $root "ИНСТРУКЦИЯ.md") (Join-Path $stage "README.md") -ErrorAction SilentlyContinue
foreach ($f in @("src-tauri\build_info.txt", "tools\build_number.txt")) { $p = Join-Path $root $f; if (Test-Path $p) { Copy-Item $p $stage } }
$files = Get-ChildItem $stage -File | ForEach-Object { $_.Name }
$target = Join-Path $stage "Proverka-Setup.exe"
$sed = @"
[Version]
Class=IEXPRESS
SEDVersion=3
[Options]
PackagePurpose=InstallApp
ShowInstallProgramWindow=0
HideExtractAnimation=1
UseLongFileName=1
InsideCompressed=0
CAB_FixedSize=0
CAB_ResvCodeSigning=0
RebootMode=N
InstallPrompt=%InstallPrompt%
DisplayLicense=%DisplayLicense%
FinishMessage=%FinishMessage%
TargetName=%TargetName%
FriendlyName=%FriendlyName%
AppLaunched=%AppLaunched%
PostInstallCmd=%PostInstallCmd%
AdminQuietInstCmd=%AdminQuietInstCmd%
UserQuietInstCmd=%UserQuietInstCmd%
SourceFiles=SourceFiles
[Strings]
InstallPrompt=
DisplayLicense=
FinishMessage=
TargetName=$target
FriendlyName=Proverka Zadaniy
AppLaunched=powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File install.ps1 -FromSetup
PostInstallCmd=<None>
AdminQuietInstCmd=
UserQuietInstCmd=
"@
$i = 0
foreach ($f in $files) { $sed += "FILE$i=`"$f`"`r`n"; $i++ }
$sed += "[SourceFiles]`r`nSourceFiles0=$stage\`r`n[SourceFiles0]`r`n"
$i = 0
foreach ($f in $files) { $sed += "%FILE$i%=`r`n"; $i++ }
$sedPath = Join-Path $stage "setup.sed"
[IO.File]::WriteAllText($sedPath, $sed, [Text.Encoding]::Default)
Write-Host "Упаковываю установщик (IExpress)..." -ForegroundColor Cyan
& "$env:SystemRoot\System32\iexpress.exe" /N /Q $sedPath | Out-Null
$wait = 0
while (-not (Test-Path $target) -and $wait -lt 120) { Start-Sleep 1; $wait++ }
if (-not (Test-Path $target)) { Write-Host "Не удалось собрать установщик" -ForegroundColor Red; if (-not $NoPause) { Read-Host "Нажмите Enter" }; exit 1 }
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$out = Join-Path $dist "Установка — Проверка заданий.exe"
Copy-Item $target $out -Force
Write-Host "Готово: $out" -ForegroundColor Green
if (-not $env:CI) { Start-Process explorer.exe -ArgumentList "/select,`"$out`"" }
if (-not $NoPause) { Read-Host "Нажмите Enter" }
