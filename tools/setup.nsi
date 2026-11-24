; Установщик «Проверки заданий» (NSIS, собирается в GitHub Actions).
; Ставит приложение для текущего пользователя в %LOCALAPPDATA%\Programs, без прав администратора:
; ярлыки на рабочем столе и в «Пуске», удаление через «Параметры → Приложения».
Unicode true
!define APP "Проверка заданий"
!define REGKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ProverkaZadaniy"
!ifndef VERSION
  !define VERSION "1.0.0"
!endif

Name "${APP}"
OutFile "..\dist\Proverka-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "Spezio · Telegram @ASpezio"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "Spezio"
VIAddVersionKey "FileDescription" "Установка — ${APP}"
VIAddVersionKey "LegalCopyright" "Spezio"

!include "MUI2.nsh"
!define MUI_ICON "..\src-tauri\icons\icon.ico"
!define MUI_UNICON "..\src-tauri\icons\icon.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "Установка «${APP}» ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "Приложение проверяет работы студентов нейросетью, ищет следы ИИ и списывание, собирает ведомость и журнал группы.$\r$\n$\r$\nРаботы, оценки и настройки хранятся только на этом компьютере (папка %APPDATA%\proverka) и при обновлении или удалении приложения не теряются."
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP}.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Запустить «${APP}»"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"

Section "Install"
  ; запущенное приложение (в том числе из трея) мешает заменить файл
  nsExec::Exec 'taskkill /IM "${APP}.exe" /F'
  Sleep 600
  SetOutPath "$INSTDIR"
  File "/oname=${APP}.exe" "..\dist\ProverkaZadaniy.exe"
  File "/oname=ИНСТРУКЦИЯ.md" "..\ИНСТРУКЦИЯ.md"
  WriteUninstaller "$INSTDIR\Удалить.exe"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${APP}.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APP}.exe"
  WriteRegStr HKCU "${REGKEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${REGKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REGKEY}" "Publisher" "Spezio"
  WriteRegStr HKCU "${REGKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${REGKEY}" "DisplayIcon" "$INSTDIR\${APP}.exe,0"
  WriteRegStr HKCU "${REGKEY}" "URLInfoAbout" "https://t.me/ASpezio"
  WriteRegStr HKCU "${REGKEY}" "UninstallString" '"$INSTDIR\Удалить.exe"'
  WriteRegDWORD HKCU "${REGKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${REGKEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM "${APP}.exe" /F'
  Sleep 600
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$INSTDIR\${APP}.exe"
  Delete "$INSTDIR\${APP}.exe.old"
  Delete "$INSTDIR\ИНСТРУКЦИЯ.md"
  Delete "$INSTDIR\Удалить.exe"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "${REGKEY}"
SectionEnd
