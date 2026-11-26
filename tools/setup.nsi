; Установщик «Проверки заданий» (NSIS, собирается в GitHub Actions).
; Ставит приложение для текущего пользователя (по умолчанию в %LOCALAPPDATA%\Programs, папку можно
; выбрать), без прав администратора: ярлыки на рабочем столе и в «Пуске» — по выбору,
; удаление через «Параметры → Приложения».
Unicode true
!define APP "Проверка заданий"
!define AUMID "com.spezio.proverka"
!define SITE "https://github.com/ASpezio/Proverka-Zadaniy"
!define REGKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ProverkaZadaniy"
!ifndef VERSION
  !define VERSION "1.0.0"
!endif

Name "${APP}"
OutFile "..\dist\Proverka-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
; при переустановке предлагается та же папка, что и в прошлый раз
InstallDirRegKey HKCU "${REGKEY}" "InstallLocation"
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
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_DIRECTORYPAGE_TEXT_TOP "Приложение будет установлено в эту папку. Права администратора не нужны, если папка доступна для записи вашей учётной записи (например, внутри вашей папки пользователя или на диске D:)."
!define MUI_FINISHPAGE_TITLE "«${APP}» установлено"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP}.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Запустить приложение"
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Посетить страницу проекта"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION OpenSite
!define MUI_FINISHPAGE_NOREBOOTSUPPORT
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"

Function OpenSite
  ExecShell "open" "${SITE}"
FunctionEnd

Section "!Приложение" SecMain
  SectionIn RO
  ; запущенное приложение (в том числе из трея) мешает заменить файл
  nsExec::Exec 'taskkill /IM "${APP}.exe" /F'
  Sleep 600
  SetOutPath "$INSTDIR"
  File "/oname=${APP}.exe" "..\dist\ProverkaZadaniy.exe"
  File "/oname=ИНСТРУКЦИЯ.md" "..\ИНСТРУКЦИЯ.md"
  WriteUninstaller "$INSTDIR\Удалить.exe"
  ; имя и значок для уведомлений Windows (приложение пишет это и само при запуске)
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${AUMID}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${REGKEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${REGKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REGKEY}" "Publisher" "Spezio"
  WriteRegStr HKCU "${REGKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${REGKEY}" "DisplayIcon" "$INSTDIR\${APP}.exe,0"
  WriteRegStr HKCU "${REGKEY}" "URLInfoAbout" "${SITE}"
  WriteRegStr HKCU "${REGKEY}" "HelpLink" "https://t.me/ASpezio"
  WriteRegStr HKCU "${REGKEY}" "UninstallString" '"$INSTDIR\Удалить.exe"'
  WriteRegDWORD HKCU "${REGKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${REGKEY}" "NoRepair" 1
SectionEnd

Section "Ярлык на рабочем столе" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${APP}.exe"
SectionEnd

Section "Ярлык в меню «Пуск»" SecStart
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APP}.exe"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "Само приложение и инструкция. Ваши данные (работы, журнал, настройки) лежат отдельно и не затрагиваются."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Значок «${APP}» на рабочем столе."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStart} "Значок в меню «Пуск» — приложение можно найти поиском Windows."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  nsExec::Exec 'taskkill /IM "${APP}.exe" /F'
  Sleep 600
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP}.lnk"
  ; файл может быть ещё занят (только что закрытая программа, антивирус) — повторяем до 10 раз
  StrCpy $0 0
  del_exe:
  Delete "$INSTDIR\${APP}.exe"
  IfFileExists "$INSTDIR\${APP}.exe" 0 del_exe_ok
  IntOp $0 $0 + 1
  IntCmp $0 10 del_exe_ok
  Sleep 500
  Goto del_exe
  del_exe_ok:
  Delete "$INSTDIR\${APP}.exe.old"
  Delete "$INSTDIR\${APP}.exe.new"
  Delete "$INSTDIR\ИНСТРУКЦИЯ.md"
  Delete "$INSTDIR\Удалить.exe"
  ; без /r: если папку выбрали неудачно (например, D:\), чужие файлы не пострадают
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\${AUMID}"
  DeleteRegKey HKCU "${REGKEY}"
SectionEnd
