; AMWAPOS NSIS installer hooks.
; Business data lives in %ProgramData%\AMWAPOS\data and is NEVER removed by
; install, upgrade or uninstall. Only the program files are managed here.

!macro NSIS_HOOK_POSTINSTALL
  ; Create the shared data directory up front with explicit permissions: the till
  ; may be used under different Windows accounts, so local Users may modify files
  ; inside it (inheritance disabled; SYSTEM and Administrators keep full control).
  ; Access to business functions is controlled by AMWAPOS staff PINs and roles.
  CreateDirectory "$COMMONAPPDATA\AMWAPOS\data"
  nsExec::Exec 'icacls "$COMMONAPPDATA\AMWAPOS" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)M"'
  ; One inbound rule only: the hub API (TCP 47800), private networks, bound to
  ; the AMWAPOS executable. Terminals join by entering the hub address shown on
  ; the hub's Sync page. WhatsApp runs inside AMWAPOS (outbound only) and OCR
  ; runs the bundled Tesseract as a child process; neither listens on a port.
  ; The UDP discovery rule of earlier versions is removed.
  nsExec::Exec 'netsh advfirewall firewall delete rule name="AMWAPOS Hub"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="AMWAPOS Hub" dir=in action=allow program="$INSTDIR\amwapos.exe" protocol=TCP localport=47800 profile=private enable=yes'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="AMWAPOS Discovery"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="AMWAPOS Hub"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="AMWAPOS Discovery"'
  DetailPrint "Business data in $COMMONAPPDATA\AMWAPOS is kept. Delete it manually only after taking a backup."
!macroend
