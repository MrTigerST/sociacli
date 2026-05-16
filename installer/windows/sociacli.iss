; Inno Setup script for sociacli
; Compile with: ISCC.exe /DAppVersion=0.1.0 installer\windows\sociacli.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#ifndef BinDir
  ; CI overrides this to point at target/<triple>/release/
  #define BinDir "..\..\target\release"
#endif

#define AppName "sociacli"
#define AppPublisher "sociacli authors"
#define AppExe "sociacli.exe"
#define AppId "{{8B5C2D9A-5C1A-4FBC-9C0F-0E9D1F8C3B11}}"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExe}
OutputDir=Output
OutputBaseFilename=sociacli-setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
LicenseFile=..\..\LICENSE
; Tell explorer to broadcast WM_SETTINGCHANGE after we edit HKCU\Environment\Path
; so new cmd / PowerShell sessions see `sociacli` without a logoff.
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Components]
Name: "core";     Description: "sociacli binary";                           Types: full compact custom; Flags: fixed
Name: "shortcut"; Description: "Start Menu + Desktop shortcuts";            Types: full compact
Name: "hotkey";   Description: "Global hotkey (Ctrl+Alt+Shift+F) opens sociacli"; Types: full
Name: "startup";  Description: "Run notification daemon (sociacli listen) on Windows startup"; Types: full
Name: "addpath";  Description: "Add sociacli to PATH (so `sociacli` works in cmd / PowerShell)"; Types: full

[Files]
Source: "{#BinDir}\{#AppExe}";            DestDir: "{app}"; Flags: ignoreversion; Components: core
Source: "{#BinDir}\sociacli-overlay.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: core
Source: "..\..\README.md";                DestDir: "{app}"; Flags: ignoreversion isreadme; Components: core
Source: "..\..\LICENSE";                  DestDir: "{app}"; Flags: ignoreversion; Components: core
; GUI-subsystem launcher for the boot daemon (no console window, AV-friendly).
Source: "{#BinDir}\sociacli-launch.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: startup

[Icons]
Name: "{group}\{#AppName}";            Filename: "{app}\{#AppExe}";    Components: shortcut
Name: "{group}\Uninstall {#AppName}";  Filename: "{uninstallexe}";      Components: shortcut
Name: "{autodesktop}\{#AppName}";      Filename: "{app}\{#AppExe}";    Components: shortcut
; Global hotkey: Windows reads the HotKey field on .lnk files placed in
; Start Menu (or on the Desktop). Ctrl+Alt+Shift+F opens a fresh sociacli REPL.
Name: "{userstartmenu}\sociacli (Ctrl+Alt+Shift+F)"; Filename: "{app}\{#AppExe}"; HotKey: "ctrl+alt+shift+f"; Components: hotkey

[Registry]
; Run the notification daemon when the user logs in. We point at the bundled
; GUI-subsystem launcher (sociacli-launch.exe) rather than the bare console
; exe: Windows allocates no console for a GUI-subsystem process, so the daemon
; starts invisibly. It's an ordinary .exe (not a wscript/.vbs), so it doesn't
; trip the antivirus heuristics that flag scripts launching hidden processes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "sociacli"; \
  ValueData: """{app}\sociacli-launch.exe"""; \
  Flags: uninsdeletevalue; Components: startup
; Append {app} to the per-user PATH. NeedsAddPath stops duplicate entries.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; \
  Check: NeedsAddPath(ExpandConstant('{app}')); \
  Flags: preservestringtype; Components: addpath

[Run]
; Start the notification daemon right now (via the GUI-subsystem launcher, so
; nothing flashes) so the global hotkey works without a logoff/reboot. Using
; the launcher rather than `runhidden` on the console exe keeps the installer
; from spawning a hidden process itself — which some antivirus heuristics
; flagged and could abort the install. `nowait` so the wizard still finishes.
Filename: "{app}\sociacli-launch.exe"; \
  Flags: nowait skipifdoesntexist skipifsilent; Components: startup
Filename: "{app}\{#AppExe}"; Description: "Launch sociacli now"; \
  Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  // Surrounding semicolons make a substring match unambiguous.
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

procedure RemoveFromPath(Target: string);
var
  OrigPath, NewPath, Lower, LowerTarget: string;
  Idx: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
    exit;
  Lower := ';' + Uppercase(OrigPath) + ';';
  LowerTarget := ';' + Uppercase(Target) + ';';
  Idx := Pos(LowerTarget, Lower);
  if Idx = 0 then exit;
  // Strip the matched segment, keeping the leading sentinel ';' and removing
  // the trailing one before unwrapping the outer semicolons.
  NewPath := Copy(OrigPath, 1, Idx - 1) + Copy(OrigPath, Idx + Length(Target), MaxInt);
  // Collapse any "::" produced by the cut.
  while Pos(';;', NewPath) > 0 do
    StringChangeEx(NewPath, ';;', ';', True);
  // Trim leading / trailing ';'.
  if (Length(NewPath) > 0) and (NewPath[1] = ';') then
    Delete(NewPath, 1, 1);
  if (Length(NewPath) > 0) and (NewPath[Length(NewPath)] = ';') then
    Delete(NewPath, Length(NewPath), 1);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', NewPath);
end;

procedure KillSociacliProcesses;
var
  ResultCode: Integer;
begin
  // Kill every sociacli process so the .exe / .dll are no longer locked.
  // `taskkill` returns non-zero when nothing matches — ignore.
  Exec(ExpandConstant('{sys}\taskkill.exe'),
    '/F /T /IM sociacli.exe', '',
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'),
    '/F /T /IM sociacli-overlay.exe', '',
    SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // usAppMutexCheck fires before any file removal — the right place to free
  // file locks. Repeat on usUninstall as a safety net.
  if (CurUninstallStep = usAppMutexCheck) or (CurUninstallStep = usUninstall) then
    KillSociacliProcesses;
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;

[UninstallDelete]
; ProjectDirs maps to %APPDATA%\sociacli\sociacli (Roaming, NOT Local) on
; Windows. Wipe both so reinstall starts from a clean slate.
Type: filesandordirs; Name: "{userappdata}\sociacli"
Type: filesandordirs; Name: "{localappdata}\sociacli"
