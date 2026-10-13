; KilatX Setup (Inno Setup 6). Build with setup\build.ps1, or:
;   ISCC.exe /DAppVersion=0.5.0 /DArch=x64 setup\KilatX.iss
;
; Installs to Program Files (administrator permission once), so only administrators can change
; KilatX.exe or put files next to it. Everything per user (settings, downloads list, browser
; connection, start with Windows) stays in %APPDATA%\KilatX and is set up by KilatX itself.
;
; Silent: KilatX-Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/DIR="…"] [/launch]
;   /launch       start KilatX after a silent install (used by KilatX's own updater)
; Uninstall silently: unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/removedata]
;   /removedata   also delete the user's KilatX settings and download list
;
; /DTest builds "KilatX-Test" (own name, folder and Installed-apps entry) for the test scripts.

#ifndef Arch
  #define Arch "x64"
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifdef Test
  #define Product "KilatX-Test"
  #define TheAppId "{{02B69550-DBD6-419F-A029-44E057A0D633}"
#else
  #define Product "KilatX"
  #define TheAppId "{{6F7BF8B4-9806-4A82-AED0-B2E178DA5054}"
#endif
#ifndef PayloadDir
  #if Arch == "arm64"
    #define PayloadDir "..\app\target\aarch64-pc-windows-msvc\release"
  #else
    #define PayloadDir "..\app\target\release"
  #endif
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

[Setup]
AppId={#TheAppId}
AppName={#Product}
AppVersion={#AppVersion}
AppVerName={#Product} {#AppVersion}
AppPublisher=KilatX
AppCopyright=© 2026 KilatX
VersionInfoVersion={#AppVersion}
VersionInfoProductName=KilatX
VersionInfoDescription=KilatX Setup
DefaultDirName={autopf}\{#Product}
UsePreviousAppDir=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=..\app\ui\icons\app.ico
WizardStyle=modern
WizardSmallImageFile=..\app\ui\icons\app-256.png
UninstallDisplayIcon={app}\KilatX.exe
UninstallDisplayName={#Product}
OutputDir={#OutDir}
OutputBaseFilename=KilatX-Setup-{#AppVersion}-{#Arch}
Compression=lzma2/ultra64
SolidCompression=yes
; KilatX is asked to save and exit first (PrepareToInstall); this catches anything left,
; e.g. KilatX running in another Windows account or a browser's connection to it.
CloseApplications=force
RestartApplications=no
SetupMutex={#Product}-Setup
SetupLogging=yes
ShowLanguageDialog=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\KilatX.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\app\docs\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The AppUserModelID keeps KilatX's name and icon on its notifications.
Name: "{autoprograms}\{#Product}"; Filename: "{app}\KilatX.exe"; Comment: "KilatX"; AppUserModelID: "KilatX.KilatX"
Name: "{autodesktop}\{#Product}"; Filename: "{app}\KilatX.exe"; Comment: "KilatX"; AppUserModelID: "KilatX.KilatX"; Tasks: desktopicon

[Run]
; As the signed-in user, not as administrator: an elevated KilatX couldn't take drag and drop
; or talk to the browser, and the files it downloads would belong to the administrator.
Filename: "{app}\KilatX.exe"; Description: "{cm:LaunchProgram,{#Product}}"; Flags: nowait postinstall skipifsilent runasoriginaluser
Filename: "{app}\KilatX.exe"; Flags: nowait runasoriginaluser; Check: LaunchAfterSilentInstall

[Code]
const
  LegacyKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#Product}';

function ParamExists(const Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
    begin
      Result := True;
      Exit;
    end;
end;

function LaunchAfterSilentInstall: Boolean;
begin
  Result := WizardSilent and ParamExists('/launch');
end;

function IsDirEmpty(const Dir: String): Boolean;
var
  F: TFindRec;
begin
  Result := True;
  if FindFirst(AddBackslash(Dir) + '*', F) then
  try
    repeat
      if (F.Name <> '.') and (F.Name <> '..') then
      begin
        Result := False;
        Exit;
      end;
    until not FindNext(F);
  finally
    FindClose(F);
  end;
end;

{ Asks the user's running KilatX to save its progress and exit, waits for it, then ends what
  else still runs from the installed KilatX.exe (e.g. a browser's connection to it). Runs the
  new KilatX.exe from the temporary folder, so the installed one is free to be replaced. }
procedure CloseKilatX;
var
  Exe, Args: String;
  Code: Integer;
begin
  ExtractTemporaryFile('KilatX.exe');
  Exe := ExpandConstant('{tmp}\KilatX.exe');
  Args := '/close "' + ExpandConstant('{app}\KilatX.exe') + '"';
  if ExecAsOriginalUser(Exe, Args, '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('KilatX /close: exit code %d', [Code]))
  else if Exec(Exe, Args, '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('KilatX /close (elevated): exit code %d', [Code]));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Dir: String;
begin
  Result := '';
  Dir := RemoveBackslash(ExpandConstant('{app}'));
  if RegValueExists(HKLM, 'SOFTWARE\{#Product}', 'InstalledBy') then
    Result := '{#Product} was installed on this PC with the MSI package (usually by your IT department). ' +
      'Update it with a newer MSI, or uninstall it first (Settings › Apps).'
  else if FileExists(Dir + '\portable.txt') or DirExists(Dir + '\data') then
    Result := Dir + ' holds a portable KilatX. Choose another folder (the portable copy keeps working).'
  else if DirExists(Dir) and not FileExists(Dir + '\KilatX.exe') and not IsDirEmpty(Dir) then
    Result := Dir + ' already contains other files. Choose an empty folder or a new one, for example ' +
      ExpandConstant('{autopf}\{#Product}') + '.';
  if Result = '' then
    CloseKilatX;
end;

{ Outside Program Files (e.g. D:\Apps\KilatX) the folder would inherit "everyone may change
  files" from the drive. Give it the same protection as Program Files: only administrators
  and Windows may change it, users may run it. }
procedure ProtectFolder(const Dir: String);
var
  PF: String;
  Code: Integer;
begin
  PF := AddBackslash(ExpandConstant('{commonpf64}'));
  if Pos(Lowercase(PF), Lowercase(AddBackslash(Dir))) = 1 then
    Exit;
  if Exec(ExpandConstant('{sys}\icacls.exe'),
      '"' + Dir + '" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F *S-1-5-32-545:(OI)(CI)RX /Q',
      '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('Protected %s: icacls exit code %d', [Dir, Code]));
end;

{ KilatX 0.4 installed itself per user in %LOCALAPPDATA%\Programs\KilatX. Its own uninstaller
  removes it (the settings and download list are kept: they are in %APPDATA%\KilatX). }
procedure RemoveUserInstall(const Dir: String);
var
  Old: String;
  Code: Integer;
begin
  if not RegQueryStringValue(HKCU, LegacyKey, 'InstallLocation', Old) then
    Exit;
  Old := RemoveBackslash(Old);
  if (CompareText(Old, Dir) = 0) or not FileExists(Old + '\KilatX.exe') then
    Exit;
  if ExecAsOriginalUser(Old + '\KilatX.exe', '/uninstall /quiet', Old, SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('Removed the per-user KilatX in %s: exit code %d', [Old, Code]));
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Dir: String;
  Code: Integer;
begin
  if CurStep <> ssPostInstall then
    Exit;
  Dir := RemoveBackslash(ExpandConstant('{app}'));
  ProtectFolder(Dir);
  RemoveUserInstall(Dir);
  { Browsers of every user of this PC reach KilatX (companies often allow only this kind). }
  if Exec(Dir + '\KilatX.exe', '/register-machine', Dir, SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('KilatX /register-machine: exit code %d', [Code]));
  { Browser connection, notification name and start with Windows, for the signed-in user. }
  if ExecAsOriginalUser(Dir + '\KilatX.exe', '/register-user', Dir, SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('KilatX /register-user: exit code %d', [Code]));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Exe, Args: String;
  Code: Integer;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  Exe := ExpandConstant('{app}\KilatX.exe');
  if not FileExists(Exe) then
    Exit;
  Exec(Exe, '/close "' + Exe + '"', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(Exe, '/unregister-machine', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Args := '/cleanup-user';
  if ParamExists('/removedata') or
     ((not UninstallSilent) and
      (MsgBox('Also delete your KilatX settings and download list?' + #13#10#13#10 +
              'Your downloaded files are not touched. Choose No to keep the settings for a later reinstall.',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES)) then
    Args := Args + ' /removedata';
  Exec(Exe, Args, '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
