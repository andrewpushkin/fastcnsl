; fastcnsl installer (Inno Setup 7). Built by scripts/package/windows-setup.sh:
;
;   ISCC /DAppVersion=0.1.N /DArch=x64|arm64 /DSourceDir=<staged files>
;        /DIconFile=<fastcnsl.ico> /O<output dir> /F<file name> fastcnsl.iss
;
; One installer per architecture. Installs for the current user without an
; admin prompt by default (%LOCALAPPDATA%\Programs\fastcnsl), so a newer
; setup, also run silently with /VERYSILENT, can update it in place.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<folder with the files to install>
#endif
#ifndef IconFile
  #error Pass /DIconFile=<path to fastcnsl.ico>
#endif
#if Arch == "x64"
  #define ArchName "x64"
  #define OtherFile "fastcnsl-windows-arm64-setup.exe"
#elif Arch == "arm64"
  #define ArchName "ARM64"
  #define OtherFile "fastcnsl-windows-x86_64-setup.exe"
#else
  #error Pass /DArch=x64 or /DArch=arm64
#endif

[Setup]
; Never change AppId: it is how a newer setup finds the installed copy.
AppId={{4FFEA6F9-A30F-4EF2-9FF2-6CF7D8C247F9}
AppName=fastcnsl
AppVersion={#AppVersion}
AppVerName=fastcnsl {#AppVersion}
VersionInfoVersion={#AppVersion}.0
AppPublisher=Andrei Pushkin
AppPublisherURL=https://github.com/andrewpushkin/fastcnsl
AppSupportURL=https://github.com/andrewpushkin/fastcnsl/issues
AppUpdatesURL=https://github.com/andrewpushkin/fastcnsl/releases
DefaultDirName={autopf}\fastcnsl
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
; Both builds start on either processor so that [Code] can name the right
; download; 64-bit install mode only on the matching one.
ArchitecturesAllowed=x64os or arm64
#if Arch == "x64"
ArchitecturesInstallIn64BitMode=x64os
#else
ArchitecturesInstallIn64BitMode=arm64
#endif
; ConPTY needs Windows 10 1809.
MinVersion=10.0.17763
; Close running fastcnsl windows (and their OpenConsole.exe) before
; replacing files, also in silent updates.
CloseApplications=force
RestartApplications=no
ChangesEnvironment=yes
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\fastcnsl.exe
UninstallDisplayName=fastcnsl
WizardStyle=modern dynamic
Compression=lzma2/max
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
WrongArch=This is the {#ArchName} installer, and this PC has a different processor.%n%nDownload {#OtherFile} instead.

[Tasks]
Name: "startmenu"; Description: "Add to Start menu"
Name: "desktopicon"; Description: "Add a desktop icon"; Flags: unchecked
Name: "addtopath"; Description: "Add fastcnsl to PATH"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[InstallDelete]
; An upgrade with a box now unchecked removes what the earlier install added.
Type: files; Name: "{autoprograms}\fastcnsl.lnk"; Tasks: not startmenu
Type: files; Name: "{autodesktop}\fastcnsl.lnk"; Tasks: not desktopicon

[Icons]
Name: "{autoprograms}\fastcnsl"; Filename: "{app}\fastcnsl.exe"; Tasks: startmenu
Name: "{autodesktop}\fastcnsl"; Filename: "{app}\fastcnsl.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\fastcnsl.exe"; Description: "{cm:LaunchProgram,fastcnsl}"; Flags: postinstall nowait skipifsilent

[Code]
function InitializeSetup(): Boolean;
begin
#if Arch == "x64"
  Result := not IsArm64;
#else
  Result := IsArm64;
#endif
  if not Result then
    SuppressibleMsgBox(CustomMessage('WrongArch'), mbCriticalError, MB_OK, IDOK);
end;

{ PATH: the user's (HKCU) on a per-user install, the machine's on an
  all-users one. Entries compare without case or a trailing backslash. }

function EnvRoot(): Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

function EnvKey(): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function NormDir(S: String): String;
begin
  Result := Uppercase(Trim(S));
  while (Length(Result) > 0) and (Result[Length(Result)] = '\') do
    SetLength(Result, Length(Result) - 1);
end;

{ Paths without any entry equal to Dir (and without empty entries); Found
  tells whether Dir was there. }
function PathWithout(Paths, Dir: String; var Found: Boolean): String;
var
  Entry: String;
  P: Integer;
begin
  Result := '';
  Found := False;
  Paths := Paths + ';';
  while Paths <> '' do
  begin
    P := Pos(';', Paths);
    Entry := Copy(Paths, 1, P - 1);
    Delete(Paths, 1, P);
    if NormDir(Entry) = NormDir(Dir) then
      Found := True
    else if Trim(Entry) <> '' then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Entry;
    end;
  end;
end;

procedure SetPathEntry(Dir: String; Add: Boolean);
var
  Paths, Rest: String;
  Found: Boolean;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Paths) then
    Paths := '';
  Rest := PathWithout(Paths, Dir, Found);
  if Add = Found then
    exit;
  if Add then
  begin
    if Rest <> '' then
      Rest := Rest + ';';
    Rest := Rest + Dir;
  end;
  if RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Rest) then
    Log('PATH updated: ' + Rest)
  else
    Log('Could not update PATH');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    SetPathEntry(ExpandConstant('{app}'), WizardIsTaskSelected('addtopath'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    SetPathEntry(ExpandConstant('{app}'), False);
end;
