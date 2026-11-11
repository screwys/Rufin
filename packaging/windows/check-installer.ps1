param([Parameter(Mandatory)][string]$Compiler)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ('Rufin installer ü ' + [guid]::NewGuid())
$destination = Join-Path $work 'installed'
$result = Join-Path $work 'update result.json'
$name = 'RufinInstallerCheck'
$registration = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$name"
New-Item -ItemType Directory $work | Out-Null

function Build-Fixture([string]$Version) {
    $stage = Join-Path $work "stage-$Version"
    New-Item -ItemType Directory (Join-Path $stage 'bin') -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $stage 'bin/rufin.exe'), $Version)
    [IO.File]::WriteAllText((Join-Path $stage 'LICENSE'), $Version)
    [IO.File]::WriteAllText((Join-Path $stage 'z-owned.txt'), $Version)
    Copy-Item (Join-Path $PSScriptRoot 'assets/rufin.ico') $stage
    if ($Version -eq '1.0.0') {
        [IO.File]::WriteAllText((Join-Path $stage 'obsolete.txt'), 'old payload')
    } else {
        New-Item -ItemType Directory (Join-Path $stage 'z-conflict') | Out-Null
        [IO.File]::WriteAllText((Join-Path $stage 'z-conflict/child.txt'), 'new payload')
    }
    $inventory = Join-Path $work "files-$Version.txt"
    cargo run --locked -p xtask -- generate windows-installer-files $stage $inventory
    if ($LASTEXITCODE -ne 0) { throw 'Could not generate installer inventory' }
    & $Compiler /V2 "/DRUFIN_STAGE_DIR=$stage" "/DRUFIN_STAGE_FILES=$stage\*" `
        "/DRUFIN_OUTPUT_DIR=$work" "/DRUFIN_ASSET_DIR=$PSScriptRoot/assets" `
        "/DRUFIN_LANGUAGES_FILE=$work/languages.nsh" "/DRUFIN_FILES_FILE=$inventory" `
        "/DRUFIN_PROJECT_NAME=$name" "/DRUFIN_DISPLAY_NAME=$name" `
        "/DRUFIN_APP_ID=io.github.screwys.$name" "/DRUFIN_VERSION=$Version" `
        "/DRUFIN_VERSION_QUAD=$Version.0" (Join-Path $PSScriptRoot 'rufin.nsi')
    if ($LASTEXITCODE -ne 0) { throw 'Could not compile installer fixture' }
}

function Run-Installer([string]$Version, [bool]$Success) {
    [IO.File]::WriteAllText($result, 'pending')
    $process = Start-Process -FilePath (Join-Path $work "$name-$Version-setup.exe") `
        -ArgumentList "/S /RUFINUPDATE=2 /RUFINRESULT=`"$result`" /D=$destination" -PassThru
    if (-not $process.WaitForExit(30000)) {
        $process.Kill()
        throw 'Installer fixture timed out'
    }
    if (($process.ExitCode -eq 0) -ne $Success) {
        throw "Unexpected installer exit code: $($process.ExitCode)"
    }
    $feedback = Get-Content -Raw $result | ConvertFrom-Json
    $expected = if ($Success) { 'installed' } else { 'failed' }
    if ($feedback.status -ne $expected -or $feedback.version -ne $Version) {
        throw 'Installer did not report its result'
    }
}

function Assert-Restored {
    foreach ($file in $script:before.Keys) {
        $path = Join-Path $destination $file
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or
            (Get-FileHash -LiteralPath $path).Hash -ne $script:before[$file]) {
            throw "Failed upgrade changed $file"
        }
    }
    if ((Get-ItemProperty $registration).DisplayVersion -ne '1.0.0') {
        throw 'Failed upgrade changed the registered version'
    }
    $after = @(Get-ChildItem -LiteralPath $destination -Recurse -File)
    if ($after.Count -ne $script:before.Count) { throw 'Failed upgrade left extra files' }
}

try {
    $languages = '!insertmacro MUI_LANGUAGE "English"' + "`n"
    $strings = Get-Content -Raw (Join-Path $PSScriptRoot 'strings.json') | ConvertFrom-Json
    foreach ($entry in $strings.PSObject.Properties) {
        $value = $entry.Value.Replace('$', '$$').Replace('"', '$\"').Replace("`n", '$\r$\n').Replace('{app_name}', '${RUFIN_DISPLAY_NAME}')
        $languages += 'LangString ' + $entry.Name + ' ${LANG_ENGLISH} "' + $value + '"' + "`n"
    }
    [IO.File]::WriteAllText((Join-Path $work 'languages.nsh'), $languages)
    Build-Fixture '1.0.0'
    Build-Fixture '2.0.0'
    Run-Installer '1.0.0' $true
    # A file where the new package needs a directory fails during extraction,
    # after the old files have already moved to the backup.
    [IO.File]::WriteAllText((Join-Path $destination 'z-conflict'), 'unrelated file')
    [IO.File]::WriteAllText((Join-Path $destination 'keep ü.txt'), 'unrelated file')
    $script:before = @{}
    foreach ($file in Get-ChildItem -LiteralPath $destination -Recurse -File) {
        $script:before[$file.FullName.Substring($destination.Length + 1)] = (Get-FileHash -LiteralPath $file.FullName).Hash
    }
    # This lock allows the runtime check to pass but interrupts the backup
    # after earlier files, including the executable, have moved.
    $locked = [IO.File]::Open((Join-Path $destination 'z-owned.txt'), 'Open', 'Read', 'Read')
    try { Run-Installer '2.0.0' $false } finally { $locked.Dispose() }
    Assert-Restored
    Run-Installer '2.0.0' $false
    Assert-Restored
    Remove-Item -LiteralPath (Join-Path $destination 'z-conflict')
    Run-Installer '2.0.0' $true
    if ((Get-Content -Raw (Join-Path $destination 'bin/rufin.exe')) -ne '2.0.0' -or
        (Test-Path (Join-Path $destination 'obsolete.txt')) -or
        (Get-Content -Raw (Join-Path $destination 'keep ü.txt')) -ne 'unrelated file') {
        throw 'Successful upgrade did not replace only the owned files'
    }
} finally {
    Remove-Item -LiteralPath $registration -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath "HKCU:\Software\$name" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path ([Environment]::GetFolderPath('StartMenu')) "Programs/$name") -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $work -Recurse -Force
}
