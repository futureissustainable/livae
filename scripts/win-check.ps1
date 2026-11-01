# Livae check for the PC, safe to run over SSH while Livae runs.
# Starts throwaway copies (--test --offscreen: no shortcuts, no tray icon, no clipboard recording, no saving,
# windows far outside every screen that never take the focus) against a fixture folder, and prints what each
# panel shows (--dump). The real history, settings, journal and shortcuts are never touched: the journal checks
# read and write journal-test.md inside the fixture folder.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File win-check.ps1 [-Exe C:\path\to\livae.exe]

param([string]$Exe = "$env:LOCALAPPDATA\Programs\Livae\livae.exe")

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "livae.exe not found at $Exe" }

$root = Join-Path $env:TEMP 'livae-check'
Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
$data = Join-Path $root 'data'
$files = Join-Path $root 'files'
foreach ($d in @("$data\clips", "$files\Projects", "$files\node_modules\pkg", "$files\.hidden")) {
    New-Item -ItemType Directory -Force -Path $d | Out-Null
}

# Plain UTF-8 without a byte-order mark, as Livae writes its own files.
$utf8 = New-Object System.Text.UTF8Encoding $false
function Write-Text([string]$path, [string]$text) { [System.IO.File]::WriteAllText($path, $text, $utf8) }

# Files to search: two real matches, one inside node_modules and one in a hidden folder (both must stay unseen).
Write-Text "$files\report 2026.pdf" 'x'
Write-Text "$files\Projects\report notes.md" 'x'
Write-Text "$files\node_modules\pkg\report-noise.js" 'x'
Write-Text "$files\.hidden\report-hidden.txt" 'x'

Write-Text "$data\settings.toml" @"
launcher = "Hyper+Space"
clipboard = "Hyper+V"
glass = 1.0
paste = true
history = 500
file_search = ['$files']
recent_apps = false
"@

# A 64 x 32 image for the image row.
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap 64, 32
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::FromArgb(255, 176, 127, 83))
$g.Dispose()
$bmp.Save("$data\clips\6.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

# One quote for the breaks, with an author line.
Write-Text "$data\quotes.md" "Breathe out.`n`nLivae check`n"

# Journals in Vesper's format: three days before today with workouts (a 3-day streak, distancing only
# yesterday), and the same plus today's entry (the prompt then edits today).
function Day([int]$back) { (Get-Date).AddDays(-$back).ToString('yyyy-MM-dd') }
$note = 'Ported the breaks and the journal to the PC, checked each screen against the Mac and kept the day calm.'
function Entry([int]$back, [string]$day, [string]$ex, [string]$dist) {
    "## $(Day $back) - 20:00`nDay: $day`n$note`nExercise: $ex`nMetacognitive distancing: $dist`n"
}
$past = @((Entry 1 'Great' 'Yes' 'Yes'), (Entry 2 'Ok' 'Yes' 'No'), (Entry 3 'Decent' 'Yes' 'No'))
Write-Text "$root\journal-past.md" ("# Daily Learnings`n`n" + ($past -join "`n"))
Write-Text "$root\journal-today.md" ("# Daily Learnings`n`n" + ((@(Entry 0 'Perfect' 'Yes' 'No') + $past) -join "`n"))

$now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$clips = @(
    @{ id = 1; kind = 'Text'; text = 'Come in every single day and just try to do a little better.'; hash = 1; time = $now - 30 },
    @{ id = 2; kind = 'Code'; text = 'cargo build --release -p livae-win'; hash = 2; time = $now - 400 },
    @{ id = 3; kind = 'Color'; text = '#B07F53'; hash = 3; time = $now - 7300 },
    @{ id = 4; kind = 'File'; text = "$files\report 2026.pdf"; hash = 4; time = $now - 90000; pinned = $true },
    @{ id = 5; kind = 'Link'; text = 'https://livae.com/'; hash = 5; time = $now - 200 },
    @{ id = 6; kind = 'Image'; image = '6.png'; w = 64; h = 32; hash = 6; time = $now - 50 }
)
Write-Text "$data\clipboard.json" ($clips | ConvertTo-Json -Depth 3)

$env:LIVAE_DATA = $data
function Check([string]$name, [string]$argline) {
    $out = Join-Path $root "$name.txt"
    $p = Start-Process -FilePath $Exe -ArgumentList "--test --offscreen $argline --dump `"$out`"" -PassThru
    $null = $p.Handle # keeps the exit code readable after the process ends
    if (-not $p.WaitForExit(25000)) {
        $p.Kill()
        Write-Output "== $name : TIMEOUT, killed"
        return
    }
    Write-Output "== $name (exit $($p.ExitCode))"
    if (Test-Path $out) { Get-Content $out -Encoding UTF8 } else { Write-Output '(no dump written)' }
}

try {
    Check 'launcher-empty' '--show'
    Check 'launcher-app' '--show --query notepad'
    Check 'launcher-files' '--show --query report'
    Check 'launcher-calc' '--show --query "2+3*4"'
    Check 'launcher-units' '--show --query "5 km to mi"'
    Check 'clipboard' '--clipboard'
    Check 'clipboard-search' '--clipboard --query cargo'
    Check 'break-mini' '--break mini'
    Check 'break-long' '--break long'
    Check 'journal-new' '--journal'
    Check 'journal-streaks' "--journal `"$root\journal-past.md`""
    Check 'journal-edit' "--journal `"$root\journal-today.md`""
} finally {
    Remove-Item Env:\LIVAE_DATA -ErrorAction SilentlyContinue
}
Write-Output "Fixture and dumps: $root"
Write-Output 'Expect: launcher-files lists "report 2026.pdf" and "report notes.md" only (no node_modules or hidden file);'
Write-Output 'clipboard lists 6 rows with the pinned file first; launcher-calc shows 14.'
Write-Output 'break-mini: strict, no controls, quote "Breathe out." by "Livae check"; break-long: time 5:00, Postpone 5 min | Skip.'
Write-Output 'journal-new: empty note, counter 0 / 90, outline Save; journal-streaks: "Worked out" 3 days;'
Write-Output 'journal-edit: mode Edit, [Perfect], the note, Worked out yes 4 days, distancing no, primary Save.'
