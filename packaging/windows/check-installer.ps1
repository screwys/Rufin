$ErrorActionPreference = 'Stop'
$installer = (Get-ChildItem "$PSScriptRoot/../../dist/Rufin.Devel-*-setup.exe").FullName
$root = Join-Path $env:TEMP 'Rufin installer check α'
$first = Join-Path $root 'First install'
$second = Join-Path $root 'Moved install'
$registration = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Rufin.Devel'

function Install-Rufin([string]$arguments, [int]$expected = 0) {
    $process = Start-Process $installer -ArgumentList $arguments -PassThru
    if (-not $process.WaitForExit(120000)) { throw 'Installer did not finish' }
    if ($process.ExitCode -ne $expected) {
        throw "Installer returned $($process.ExitCode), expected $expected"
    }
}

function Assert-File([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing file: $path" }
}

New-Item -ItemType Directory -Force "$first/bin", "$first/share" | Out-Null
foreach ($path in @('personal.txt', 'bin/personal.txt', 'share/personal.txt')) {
    Set-Content -LiteralPath (Join-Path $first $path) -Value 'keep this file'
}
Install-Rufin "/S /D=$first"
foreach ($path in @('bin/rufin.exe', 'LICENSE', 'rufin.ico', 'install-files.txt', 'Uninstall.exe',
                     'personal.txt', 'bin/personal.txt', 'share/personal.txt')) {
    Assert-File (Join-Path $first $path)
}
if ((Get-ItemProperty $registration).InstallLocation -ne $first) { throw 'Wrong registered destination' }
$shell = New-Object -ComObject WScript.Shell
$shortcutPath = Join-Path ([Environment]::GetFolderPath('StartMenu')) 'Programs/Rufin (Development)/Rufin (Development).lnk'
Assert-File $shortcutPath
$shortcut = $shell.CreateShortcut($shortcutPath)
if ($shortcut.TargetPath -ne "$first\bin\rufin.exe") { throw 'Wrong shortcut target' }
if ($shortcut.WorkingDirectory -ne $first) { throw 'Wrong shortcut working directory' }

# Simulate a file owned by the previous package but absent from the new one.
Set-Content -LiteralPath "$first/bin/obsolete-owned.dll" -Value 'old payload'
[IO.File]::AppendAllText("$first/install-files.txt", "Fbin\obsolete-owned.dll`r`n", [Text.Encoding]::Unicode)
Install-Rufin '/S'
if (Test-Path -LiteralPath "$first/bin/obsolete-owned.dll") { throw 'Obsolete owned file survived reinstall' }
foreach ($path in @('personal.txt', 'bin/personal.txt', 'share/personal.txt')) {
    Assert-File (Join-Path $first $path)
}
Install-Rufin "/S /D=$first\Nested" 7
Assert-File "$first/bin/rufin.exe"

$app = Start-Process "$first/bin/rufin.exe" -PassThru
try {
    Start-Sleep -Seconds 5
    if ($app.HasExited) { throw "Packaged Rufin exited at startup: $($app.ExitCode)" }
    $before = (Get-FileHash "$first/bin/rufin.exe").Hash
    Install-Rufin '/S' 2
    if ((Get-FileHash "$first/bin/rufin.exe").Hash -ne $before) { throw 'Running executable was changed' }
    if (Test-Path -LiteralPath "$first/bin/rufin.exe.rufin-install") { throw 'Running executable was renamed' }
} finally {
    if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force; $app.WaitForExit() }
}

Install-Rufin "/S /D=$second"
Assert-File "$second/bin/rufin.exe"
if (Test-Path -LiteralPath "$first/bin/rufin.exe") { throw 'Old payload survived relocation' }
foreach ($path in @('personal.txt', 'bin/personal.txt', 'share/personal.txt')) {
    Assert-File (Join-Path $first $path)
}
if ((Get-ItemProperty $registration).InstallLocation -ne $second) { throw 'Relocation was not registered' }
if ($shell.CreateShortcut($shortcutPath).TargetPath -ne "$second\bin\rufin.exe") { throw 'Shortcut was not relocated' }

Set-Content -LiteralPath "$second/bin/personal.txt" -Value 'keep this file'
Set-Content -LiteralPath "$second/personal.txt" -Value 'keep this file'
$uninstaller = Start-Process "$second/Uninstall.exe" -ArgumentList '/S' -PassThru
$uninstaller.WaitForExit()
$deadline = (Get-Date).AddSeconds(60)
while ((Test-Path $registration) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
if (Test-Path $registration) { throw 'Uninstaller left registration behind' }
if (Test-Path -LiteralPath "$second/bin/rufin.exe") { throw 'Uninstaller left the payload behind' }
if (Test-Path -LiteralPath $shortcutPath) { throw 'Uninstaller left its shortcut behind' }
Assert-File "$second/bin/personal.txt"
Assert-File "$second/personal.txt"
Write-Host 'Fresh install, custom Unicode path, registered reinstall, obsolete-file cleanup, running-app guard, relocation, shortcuts, and uninstall passed.'
