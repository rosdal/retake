# Fails if the source names an API the anti-cheat rules forbid (see the roadmap's
# "Anti-cheat safety" tab). Scans Rust sources and manifests under crates/.
$ErrorActionPreference = 'Stop'
$forbidden = @(
    'ReadProcessMemory', 'WriteProcessMemory', 'VirtualAllocEx', 'CreateRemoteThread',
    'SetWindowsHookEx', 'SendInput', 'keybd_event', 'mouse_event', 'NtReadVirtualMemory',
    'PROCESS_VM_READ', 'PROCESS_ALL_ACCESS'
)
$root = Join-Path $PSScriptRoot '..\crates'
$files = Get-ChildItem -Path $root -Recurse -File -Include *.rs, Cargo.toml
$hits = $files | Select-String -SimpleMatch -Pattern $forbidden
if ($hits) {
    $hits | ForEach-Object { Write-Host "$($_.Path):$($_.LineNumber): $($_.Line.Trim())" }
    Write-Host "Forbidden API found ($(@($hits).Count) hit(s)); see the Anti-cheat safety rules."
    exit 1
}
Write-Host "No forbidden APIs in $(@($files).Count) files."
