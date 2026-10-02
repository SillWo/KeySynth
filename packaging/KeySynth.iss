#define MyAppName "KeySynth"
#define MyAppVersion "0.2.0"
#ifndef SourceExe
  #error "Pass /DSourceExe=... to the installer build script."
#endif
#ifndef OutputDirectory
  #define OutputDirectory "dist"
#endif

[Setup]
AppId={{B2E508B4-8A4D-496D-85CC-7F89C4A2E7A1}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=KeySynth
AppPublisherURL=https://github.com/SillWo/KeySynth
AppSupportURL=https://github.com/SillWo/KeySynth
DefaultDirName={localappdata}\Programs\KeySynth
DefaultGroupName=KeySynth
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
AppMutex=Local\KeySynth-Application
CloseApplications=yes
CloseApplicationsFilter=KeySynth.exe
Uninstallable=yes
UninstallDisplayName=KeySynth
UninstallDisplayIcon={app}\KeySynth.exe
SetupIconFile=..\resources\keysynth.ico
OutputDir={#OutputDirectory}
OutputBaseFilename=KeySynth-0.2.0-Setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
DisableWelcomePage=no
SetupLogging=yes
ChangesAssociations=no
ChangesEnvironment=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
english.DesktopShortcutTask=Create a desktop shortcut
english.AdditionalShortcuts=Additional shortcuts:
english.LaunchKeySynth=Launch KeySynth
russian.DesktopShortcutTask=Создать ярлык на рабочем столе
russian.AdditionalShortcuts=Дополнительные ярлыки:
russian.LaunchKeySynth=Запустить KeySynth

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopShortcutTask}"; GroupDescription: "{cm:AdditionalShortcuts}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "KeySynth.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\KeySynth"; Filename: "{app}\KeySynth.exe"
Name: "{autodesktop}\KeySynth"; Filename: "{app}\KeySynth.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\KeySynth.exe"; Description: "{cm:LaunchKeySynth}"; Flags: postinstall nowait skipifsilent

[Code]
function CommandTargetExecutable(const CommandLine: string): string;
var
  Value: string;
  ClosingQuote: Integer;
  SpaceAt: Integer;
begin
  Value := Trim(CommandLine);
  if (Length(Value) > 0) and (Value[1] = '"') then begin
    Delete(Value, 1, 1);
    ClosingQuote := Pos('"', Value);
    if ClosingQuote > 0 then
      Result := Copy(Value, 1, ClosingQuote - 1)
    else
      Result := Value;
  end else begin
    SpaceAt := Pos(' ', Value);
    if SpaceAt > 0 then
      Result := Copy(Value, 1, SpaceAt - 1)
    else
      Result := Value;
  end;
end;

procedure RemoveOwnedAutostartValue();
var
  ExistingCommand: string;
  InstalledExe: string;
begin
  if RegQueryStringValue(HKEY_CURRENT_USER,
    'Software\Microsoft\Windows\CurrentVersion\Run', 'KeySynth', ExistingCommand) then begin
    InstalledExe := ExpandConstant('{app}\KeySynth.exe');
    if CompareText(ExpandFileName(CommandTargetExecutable(ExistingCommand)),
      ExpandFileName(InstalledExe)) = 0 then
      RegDeleteValue(HKEY_CURRENT_USER,
        'Software\Microsoft\Windows\CurrentVersion\Run', 'KeySynth');
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveOwnedAutostartValue();
end;
