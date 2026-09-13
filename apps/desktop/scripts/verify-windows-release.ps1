param(
    [Parameter(Mandatory = $true)][string]$Msi,
    [Parameter(Mandatory = $true)][ValidateSet('x64', 'arm64')][string]$Architecture,
    [Parameter(Mandatory = $true)][string]$Version
)
$ErrorActionPreference = 'Stop'
$msiPath = (Resolve-Path -LiteralPath $Msi).Path
$dark = Join-Path $env:LOCALAPPDATA 'tauri/WixTools314/dark.exe'
if (-not (Test-Path -LiteralPath $dark)) { throw 'WiX dark.exe is unavailable; build an MSI with Tauri first.' }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$dumpbin = @(& $vswhere -latest -products '*' -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe') | Sort-Object | Select-Object -Last 1
if (-not $dumpbin) { throw 'Visual Studio dumpbin is required to verify runtime DLL dependencies.' }
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$inspection = Join-Path $temporaryRoot ('ez-assistant-msi-inspect-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $inspection | Out-Null
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $null
function Read-MsiRows([string]$Query, [int]$Columns) {
    $view = $database.OpenView($Query)
    try {
        [void]$view.Execute()
        while ($record = $view.Fetch()) {
            try {
                $values = @()
                for ($column = 1; $column -le $Columns; $column++) { $values += $record.StringData($column) }
                ,$values
            } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) }
        }
    } finally {
        [void]$view.Close()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
    }
}
try {
    # Read-only MSI database, never an application database or an installation operation.
    $database = $installer.OpenDatabase($msiPath, 0)
    $properties = @{}
    Read-MsiRows 'SELECT `Property`, `Value` FROM `Property`' 2 | ForEach-Object { $properties[$_[0]] = $_[1] }
    if ($properties['ProductVersion'] -ne $Version) { throw 'MSI product version mismatch.' }
    if ($properties['ALLUSERS']) { throw 'MSI must install only for the current user.' }
    $summary = $database.SummaryInformation(0)
    try {
        $expectedTemplate = if ($Architecture -eq 'x64') { 'x64;0' } else { 'Arm64;0' }
        if ($summary.Property(7) -ine $expectedTemplate) { throw 'MSI architecture or language mismatch.' }
    } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($summary) }
    if ($properties['ProductLanguage'] -ne '1033') { throw 'MSI product language mismatch.' }
    $directories = @{}
    Read-MsiRows 'SELECT `Directory`, `Directory_Parent` FROM `Directory`' 2 | ForEach-Object { $directories[$_[0]] = $_[1] }
    if ($directories['INSTALLDIR'] -ne 'LocalAppDataFolder') { throw 'Installation root must be LocalAppDataFolder.' }
    $files = @(Read-MsiRows 'SELECT `File`, `FileName` FROM `File`' 2)
    $names = @($files | ForEach-Object { ($_[1] -split '\|')[-1] })
    if ($names.Count -ne 2 -or $names -notcontains 'ez-assistant-desktop.exe' -or $names -notcontains 'ez-assistant-runtime.exe') {
        throw 'MSI must contain exactly the Desktop and Runtime Host executables.'
    }
    $components = @(Read-MsiRows 'SELECT `Component`, `Attributes` FROM `Component`' 2)
    foreach ($component in $components) {
        if (([int]$component[1] -band 4) -eq 0) { throw "Per-user component lacks registry KeyPath: $($component[0])" }
    }
    & $dark -nologo -x $inspection $msiPath -o (Join-Path $inspection 'product.wxs')
    if ($LASTEXITCODE -ne 0) { throw "MSI extraction failed: $LASTEXITCODE" }
    $expectedMachine = if ($Architecture -eq 'x64') { 0x8664 } else { 0xaa64 }
    $payloads = @(Get-ChildItem -LiteralPath (Join-Path $inspection 'File') -File -Recurse)
    if ($payloads.Count -ne 2) { throw "Expected Desktop and Host payloads; found $($payloads.Count)." }
    $desktopId = ($files | Where-Object { ($_[1] -split '\|')[-1] -eq 'ez-assistant-desktop.exe' })[0]
    foreach ($file in $payloads) {
        $stream = [IO.File]::OpenRead($file.FullName)
        try {
            $header = New-Object byte[] 4096
            $length = $stream.Read($header, 0, $header.Length)
            $offset = [BitConverter]::ToInt32($header, 60)
            if ($length -lt 64 -or $offset -lt 0 -or $offset + 6 -gt $length -or
                [BitConverter]::ToUInt16($header, 0) -ne 0x5a4d -or
                [BitConverter]::ToUInt32($header, $offset) -ne 0x4550 -or
                [BitConverter]::ToUInt16($header, $offset + 4) -ne $expectedMachine) { throw "Wrong PE architecture: $($file.Name)" }
            if ($file.Name -eq $desktopId -and [BitConverter]::ToUInt16($header, $offset + 24 + 68) -ne 2) {
                throw 'Desktop must use the Windows GUI subsystem, not the console subsystem.'
            }
        } finally { $stream.Dispose() }
        $dependencies = & $dumpbin /nologo /dependents $file.FullName
        if ($LASTEXITCODE -ne 0) { throw "PE dependency inspection failed: $($file.Name)" }
        if ($dependencies -match '(?i)^\s*(vcruntime\d[^\s]*|msvcp\d[^\s]*|concrt\d[^\s]*)\.dll\s*$') {
            throw "Payload requires an unbundled VC++ runtime DLL: $($file.Name)"
        }
    }
    $indexPath = Join-Path $PSScriptRoot '../dist/index.html'
    $webManifest = Get-Content -Raw (Join-Path $PSScriptRoot '../dist/host-web-manifest.json') | ConvertFrom-Json
    if ($webManifest.version -ne $Version) { throw 'Embedded Web build version mismatch.' }
    $byteEncoding = [Text.Encoding]::GetEncoding(28591)
    $webIndex = $byteEncoding.GetString([IO.File]::ReadAllBytes($indexPath))
    # Host embeds the unmodified index; Tauri transforms/compresses its own assets.
    $hostId = ($files | Where-Object { ($_[1] -split '\|')[-1] -eq 'ez-assistant-runtime.exe' })[0]
    $hostPayload = @($payloads | Where-Object { $_.Name -eq $hostId })
    if ($hostPayload.Count -ne 1) { throw 'Extracted Host payload is missing or ambiguous.' }
    foreach ($file in $hostPayload) {
        if (-not $byteEncoding.GetString([IO.File]::ReadAllBytes($file.FullName)).Contains($webIndex)) {
            throw "Built Web index is missing from payload: $($file.Name)"
        }
    }
    Write-Output "Verified MSI $Architecture $Version`: current-user scope, version, both PE architectures, GUI subsystem, no external VC++ runtime DLL, and Host embedded Web."
} finally {
    if ($database) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($database) }
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
    $resolvedInspection = [IO.Path]::GetFullPath($inspection)
    if (-not $resolvedInspection.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolvedInspection) -notlike 'ez-assistant-msi-inspect-*') { throw 'Unsafe inspection cleanup path.' }
    Remove-Item -LiteralPath $resolvedInspection -Recurse -Force
}
