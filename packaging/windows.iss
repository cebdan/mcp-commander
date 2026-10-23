#define AppVersion GetEnv("MCP_COMMANDER_VERSION")
#define ReleaseLabel GetEnv("MCP_COMMANDER_RELEASE_LABEL")

[Setup]
SourceDir=..
AppId={{8A6841E2-5284-4BB2-8CF5-1FC652117C31}
AppName=MCP Commander
AppVersion={#AppVersion}
AppPublisher=MCP Commander contributors
AppPublisherURL=https://github.com/cebdan/mcp-commander
DefaultDirName={localappdata}\Programs\MCP Commander
DefaultGroupName=MCP Commander
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=dist
OutputBaseFilename=MCP-Commander-{#ReleaseLabel}-Windows-x64-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\mcp-commander.exe

[Files]
Source: "target\release\mcp-commander.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\MCP Commander"; Filename: "{app}\mcp-commander.exe"
Name: "{autodesktop}\MCP Commander"; Filename: "{app}\mcp-commander.exe"; Tasks: desktopicon

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Run]
Filename: "{app}\mcp-commander.exe"; Description: "Launch MCP Commander"; Flags: nowait postinstall skipifsilent
