#define SourceDir GetEnv("LETRECOVERY_INSTALLER_SOURCE")
#define OutputDir GetEnv("LETRECOVERY_INSTALLER_OUTPUT")
#define AppVersion GetEnv("LETRECOVERY_INSTALLER_VERSION")
#define AppDisplayVersion GetEnv("LETRECOVERY_INSTALLER_DISPLAY_VERSION")
#define AppIcon GetEnv("LETRECOVERY_INSTALLER_ICON")

#if SourceDir == ""
  #error "LETRECOVERY_INSTALLER_SOURCE is not set"
#endif
#if OutputDir == ""
  #error "LETRECOVERY_INSTALLER_OUTPUT is not set"
#endif
#if AppVersion == ""
  #error "LETRECOVERY_INSTALLER_VERSION is not set"
#endif
#if AppIcon == ""
  #error "LETRECOVERY_INSTALLER_ICON is not set"
#endif

[Setup]
AppId={{F0B9EACD-36A4-4D12-B07E-4D0CC87B4798}
AppName=LetRecovery
AppVersion={#AppDisplayVersion}
AppVerName=LetRecovery {#AppDisplayVersion}
AppPublisher=NORMAL-EX
AppPublisherURL=https://letrecovery.net/
AppSupportURL=https://letrecovery.net/
AppUpdatesURL=https://letrecovery.net/
AppCopyright=Copyright (C) 2026 NORMAL-EX
DefaultDirName={autopf}\LetRecovery
DefaultGroupName=LetRecovery
DisableProgramGroupPage=yes
AllowNoIcons=yes
LicenseFile=LICENSE.zh-CN.txt
InfoBeforeFile=NOTICE.zh-CN.txt
OutputDir={#OutputDir}
OutputBaseFilename=LetRecovery-Setup-x64
SetupIconFile={#AppIcon}
UninstallDisplayIcon={app}\LetRecovery.exe
UninstallDisplayName=LetRecovery
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.10240
WizardStyle=modern dynamic windows11 hidebevels includetitlebar
WizardSizePercent=110
DefaultDialogFontName=Microsoft YaHei UI
Compression=lzma2/ultra64
SolidCompression=yes
CompressionThreads=auto
LZMAUseSeparateProcess=yes
CloseApplications=yes
CloseApplicationsFilter=LetRecovery.exe
RestartApplications=no
UsePreviousAppDir=yes
UsePreviousGroup=yes
UsePreviousTasks=yes
SetupLogging=yes
DisableWelcomePage=no
ShowLanguageDialog=no
LanguageDetectionMethod=uilanguage
VersionInfoVersion={#AppVersion}
VersionInfoCompany=NORMAL-EX
VersionInfoDescription=LetRecovery offline installer
VersionInfoProductName=LetRecovery
VersionInfoProductVersion={#AppDisplayVersion}
VersionInfoCopyright=Copyright (C) 2026 NORMAL-EX

[Languages]
Name: "chinesesimp"; MessagesFile: "languages\ChineseSimplified.isl"; LicenseFile: "LICENSE.zh-CN.txt"; InfoBeforeFile: "NOTICE.zh-CN.txt"

[LangOptions]
DialogFontName=Microsoft YaHei UI
DialogFontSize=9
WelcomeFontName=Microsoft YaHei UI
WelcomeFontSize=14

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Excludes: "config.json"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#SourceDir}\config.json"; DestDir: "{app}"; Flags: onlyifdoesntexist

[Icons]
Name: "{group}\LetRecovery"; Filename: "{app}\LetRecovery.exe"; WorkingDir: "{app}"
Name: "{group}\卸载 LetRecovery"; Filename: "{uninstallexe}"
Name: "{autodesktop}\LetRecovery"; Filename: "{app}\LetRecovery.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\LetRecovery.exe"; Description: "启动 LetRecovery"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\config.json"
Type: dirifempty; Name: "{app}"
