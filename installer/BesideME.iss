; BesideME setup wizard (Inno Setup 6.7+). Build with installer\build.ps1.
;
; Per-user install (no admin rights needed):
;   program files  -> %LOCALAPPDATA%\Programs\BesideME
;   settings       -> %APPDATA%\BesideME\config.toml + .env (webhooks, from the wizard)
;   state & logs   -> %LOCALAPPDATA%\BesideME\
; Optional scheduled task "BesideME" runs `BesideME.exe --check` every 30 minutes
; (windowless, so nothing flashes on screen).

#ifndef AppVersion
  #error AppVersion must be supplied by installer/build.ps1
#endif
#define AppName "BesideME"
#define AppExe "BesideME.exe"
#define TaskName "BesideME"

[Setup]
AppId={{6F0C2B7E-4D51-4E8A-9B3C-2A7D9E1F5C40}
AppName={#AppName}
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=BesideME-Setup-{#AppVersion}
SetupIconFile=..\assets\yamada.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern dynamic
DisableWelcomePage=no
WizardImageFile=..\assets\wizard-large-164x314.png,..\assets\wizard-large-328x628.png
WizardImageFileDynamicDark=..\assets\wizard-large-164x314.png,..\assets\wizard-large-328x628.png
WizardSmallImageFile=..\assets\wizard-small-55.png,..\assets\wizard-small-110.png
WizardSmallImageFileDynamicDark=..\assets\wizard-small-55.png,..\assets\wizard-small-110.png
Compression=lzma2/max
SolidCompression=yes
; The app ignores normal close requests by design (only its red button closes it), so
; Restart Manager can't close it; [Code] ends it explicitly instead.
CloseApplications=no
RestartApplications=no

[Tasks]
Name: "background"; Description: "Check for new chapters every 30 minutes in the background (recommended)"; GroupDescription: "Background checks:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\besideme-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\.env.example"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\yamada-256.png"; DestDir: "{app}\assets"; Flags: ignoreversion
Source: "..\config.example.toml"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\fonts\OFL.txt"; DestDir: "{app}"; DestName: "Poppins-OFL.txt"; Flags: ignoreversion
; Fresh settings file; never overwrite the user's list of manga on upgrade.
Source: "config.default.toml"; DestDir: "{userappdata}\BesideME"; DestName: "config.toml"; Flags: onlyifdoesntexist uninsneveruninstall

[InstallDelete]
Type: files; Name: "{app}\MangaWatcher.exe"
Type: files; Name: "{app}\manga-watcher.exe"
Type: files; Name: "{autoprograms}\Manga Watcher.lnk"
Type: files; Name: "{autodesktop}\Manga Watcher.lnk"

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Open {#AppName} to add your first manga"; Flags: nowait postinstall skipifsilent

[Code]
const
  WebhookPrefixes = 'https://discord.com/api/webhooks/|https://discordapp.com/api/webhooks/|https://ptb.discord.com/api/webhooks/|https://canary.discord.com/api/webhooks/';

var
  WebhookPage: TInputQueryWizardPage;

function SettingsDir(): String;
begin
  Result := ExpandConstant('{userappdata}\BesideME');
end;

function EnvPath(): String;
begin
  Result := SettingsDir() + '\.env';
end;

function IsDiscordWebhook(Url: String): Boolean;
var
  Rest, Prefix: String;
  P: Integer;
begin
  Result := False;
  Rest := WebhookPrefixes;
  while Rest <> '' do
  begin
    P := Pos('|', Rest);
    if P = 0 then
    begin
      Prefix := Rest;
      Rest := '';
    end else begin
      Prefix := Copy(Rest, 1, P - 1);
      Rest := Copy(Rest, P + 1, Length(Rest));
    end;
    if (Length(Url) > Length(Prefix) + 10) and (CompareText(Copy(Url, 1, Length(Prefix)), Prefix) = 0) then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

procedure InitializeWizard();
begin
  WebhookPage := CreateInputQueryPage(wpSelectTasks,
    'Discord webhooks',
    'Set up Discord now or later in BesideME Settings.',
    'In Discord: Channel settings > Integrations > Webhooks > New Webhook > Copy Webhook URL.' + #13#10 +
    'The URLs are saved only on this computer, in ' + SettingsDir() + '\.env');
  WebhookPage.Add('Updates webhook (Yamada):', True);
  WebhookPage.Add('Health webhook (Sasaki, optional):', True);
  WebhookPage.Values[0] := ExpandConstant('{param:WEBHOOK|}');
  WebhookPage.Values[1] := ExpandConstant('{param:HEALTHWEBHOOK|}');
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = WebhookPage.ID) and
    (FileExists(EnvPath()) or
     FileExists(ExpandConstant('{userappdata}\manga-watcher\.env')));
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Updates, Health: String;
begin
  Result := True;
  { Silent installs have nobody to answer a validation prompt: never block them.
    Webhooks can be configured in the desktop app. }
  if WizardSilent() then
    Exit;
  if CurPageID = WebhookPage.ID then
  begin
    Updates := Trim(WebhookPage.Values[0]);
    Health := Trim(WebhookPage.Values[1]);
    if (Updates = '') and (Health = '') then
      Exit;
    if not IsDiscordWebhook(Updates) then
    begin
      MsgBox('Please paste the Discord webhook URL for the new chapters channel.' + #13#10 +
             'It starts with https://discord.com/api/webhooks/', mbError, MB_OK);
      Result := False;
    end else if (Health <> '') and not IsDiscordWebhook(Health) then
    begin
      MsgBox('The health alerts webhook doesn''t look like a Discord webhook URL. Leave it empty or fix it.',
             mbError, MB_OK);
      Result := False;
    end else if (Health <> '') and (CompareText(Health, Updates) = 0) then
    begin
      MsgBox('Use a different channel (webhook) for health alerts, or leave it empty.', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

procedure EndRunningApp();
var
  Code: Integer;
begin
  { The window refuses normal close requests by design, so end it directly. }
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  OldSettings, Name: String;
begin
  EndRunningApp();
  if FileExists(ExpandConstant('{app}\MangaWatcher.exe')) then
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM MangaWatcher.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
  OldSettings := ExpandConstant('{userappdata}\manga-watcher');
  Name := 'config.toml';
  if FileExists(OldSettings + '\' + Name) and not FileExists(SettingsDir() + '\' + Name) then
    if not ForceDirectories(SettingsDir()) or
       not CopyFile(OldSettings + '\' + Name, SettingsDir() + '\' + Name, True) then
      Result := 'Could not copy the previous watchlist.';
  if FileExists(OldSettings + '\.env') and not FileExists(EnvPath()) then
    if not ForceDirectories(SettingsDir()) or not CopyFile(OldSettings + '\.env', EnvPath(), True) then
      Result := 'Could not copy the previous Discord settings.';
end;

procedure WriteEnvFile();
var
  Content: String;
begin
  { Existing settings are edited in the app, which also preserves per-series keys. }
  if FileExists(EnvPath()) or not IsDiscordWebhook(Trim(WebhookPage.Values[0])) then
    Exit;
  ForceDirectories(SettingsDir());
  Content :=
    '# BesideME secrets. Written by the installer; never share or commit this file.' + #13#10 +
    'BESIDEME_WEBHOOK=' + Trim(WebhookPage.Values[0]) + #13#10;
  if IsDiscordWebhook(Trim(WebhookPage.Values[1])) and
     (CompareText(Trim(WebhookPage.Values[0]), Trim(WebhookPage.Values[1])) <> 0) then
    Content := Content + 'BESIDEME_HEALTH_WEBHOOK=' + Trim(WebhookPage.Values[1]) + #13#10;
  if not SaveStringToFile(EnvPath(), Content, False) then
    MsgBox('Could not save the webhooks to ' + EnvPath(), mbError, MB_OK);
end;

procedure ConfigureScheduledTask();
var
  Code: Integer;
  Params: String;
begin
  if WizardIsTaskSelected('background') then
  begin
    Params := '/Create /F /SC MINUTE /MO 30 /TN "{#TaskName}" /TR "\"' +
              ExpandConstant('{app}\{#AppExe}') + '\" --check"';
    if not Exec(ExpandConstant('{sys}\schtasks.exe'), Params, '', SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      MsgBox('Could not create the background check task (code ' + IntToStr(Code) + ').' + #13#10 +
             'You can still use "Check now" in the app.', mbInformation, MB_OK);
  end else
    Exec(ExpandConstant('{sys}\schtasks.exe'), '/Delete /F /TN "{#TaskName}"', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\schtasks.exe'), '/Delete /F /TN "Manga Watcher"', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WriteEnvFile();
    ConfigureScheduledTask();
  end;
end;

function InitializeUninstall(): Boolean;
begin
  EndRunningApp();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
    Exec(ExpandConstant('{sys}\schtasks.exe'), '/Delete /F /TN "{#TaskName}"', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent() then
    if MsgBox('Also delete your BesideME settings (watched manga list and Discord webhooks) and history?',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(SettingsDir(), True, True, True);
      DelTree(ExpandConstant('{localappdata}\BesideME'), True, True, True);
    end;
end;
