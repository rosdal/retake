# Fails if the source names an API the anti-cheat rules forbid (see the roadmap's
# "Anti-cheat safety" rules). Scans Rust sources and manifests under crates/.
# What the built exe actually links (dependencies included) is checked by
# crates/retake/tests/imports.rs, which is skipped here because it lists the names.
$ErrorActionPreference = 'Stop'
$forbidden = @(
    'ReadProcessMemory', 'WriteProcessMemory', 'VirtualAllocEx', 'CreateRemoteThread',
    'SetWindowsHookEx', 'SendInput', 'keybd_event', 'mouse_event', 'NtReadVirtualMemory',
    'PROCESS_VM_READ', 'PROCESS_ALL_ACCESS'
)
# Files allowed to call OpenProcess, relative to crates/ with forward slashes (none today),
# e.g. 'retake/src/process_query.rs'. Such a file may only ask for
# PROCESS_QUERY_LIMITED_INFORMATION.
$openProcessAllowed = @()

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\crates')).Path
$guard = Join-Path $root 'retake\tests\imports.rs'
$files = Get-ChildItem -Path $root -Recurse -File -Include *.rs, Cargo.toml |
    Where-Object { $_.FullName -ne $guard }
$problems = @()

$problems += $files | Select-String -SimpleMatch -Pattern $forbidden |
    ForEach-Object { "$($_.Path):$($_.LineNumber): forbidden API: $($_.Line.Trim())" }

foreach ($hit in ($files | Select-String -SimpleMatch -Pattern 'OpenProcess')) {
    $rel = $hit.Path.Substring($root.Length + 1).Replace('\', '/')
    if ($openProcessAllowed -notcontains $rel) {
        $problems += "$($hit.Path):$($hit.LineNumber): OpenProcess outside the allowed files: $($hit.Line.Trim())"
    }
}
foreach ($rel in $openProcessAllowed) {
    $path = Join-Path $root $rel
    if (-not (Test-Path $path)) { continue }
    $problems += Select-String -Path $path -Pattern 'PROCESS_[A-Z_]+' -AllMatches -CaseSensitive |
        Where-Object { $_.Matches | Where-Object { $_.Value -ne 'PROCESS_QUERY_LIMITED_INFORMATION' } } |
        ForEach-Object { "$($_.Path):$($_.LineNumber): only PROCESS_QUERY_LIMITED_INFORMATION is allowed: $($_.Line.Trim())" }
}

if ($problems) {
    $problems | ForEach-Object { Write-Host $_ }
    Write-Host "$(@($problems).Count) anti-cheat problem(s); see the Anti-cheat safety rules."
    exit 1
}
Write-Host "No forbidden APIs in $(@($files).Count) files."
