# Fresh-process cold timings. One pdf_measure.exe per PDF.
# Example:
#   .\cold_measure.ps1 -Exe .\target\release\pdf_measure.exe -Pdfs (Get-ChildItem .\fixtures\*.pdf).FullName

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string[]]$Pdfs,
    [string]$OutDir = "cold-results",
    [int]$Width = 900,
    [int]$Height = 1200,
    [int]$TimeoutSecs = 120
)

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$failed = 0
foreach ($pdf in $Pdfs) {
    $name = [IO.Path]::GetFileNameWithoutExtension($pdf)
    $out = Join-Path $OutDir "$name.json"
    Write-Host "cold $pdf -> $out"
    & $Exe --cold --json-only --width $Width --height $Height --timeout-secs $TimeoutSecs --out $out $pdf
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "$pdf exited $LASTEXITCODE"
        $failed += 1
    }
}
if ($failed -gt 0) {
    exit 1
}
