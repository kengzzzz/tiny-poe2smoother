OutFile "poe2smoother-windows-x86_64.exe"
SilentInstall silent
RequestExecutionLevel user

!define APP_NAME "poe2smoother"
!define BUNDLE_DIR "$LOCALAPPDATA\${APP_NAME}\bundle"

Section
    SetOutPath "${BUNDLE_DIR}"
    
    File "poe2smoother-gui.exe"
    File "libstdc++-6.dll"
    File "libgcc_s_seh-1.dll"
    File "libwinpthread-1.dll"
    
    Exec '"${BUNDLE_DIR}\poe2smoother-gui.exe"'
SectionEnd
