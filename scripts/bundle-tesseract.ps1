# Bundle Tesseract OCR (the OCR worker's engine) into the Windows installer.
# Installs the UB Mannheim build with Chocolatey, then copies the executable
# and its DLLs to src-tauri/resources/tesseract (models come from ocr/models,
# so Tesseract's own tessdata is not bundled).
$ErrorActionPreference = 'Stop'
choco install tesseract --no-progress -y | Out-Null
$src = @("$env:ProgramFiles\Tesseract-OCR", "${env:ProgramFiles(x86)}\Tesseract-OCR") | Where-Object { Test-Path "$_\tesseract.exe" } | Select-Object -First 1
if (-not $src) { throw "Tesseract was not installed" }
$dst = "src-tauri/resources/tesseract"
New-Item -ItemType Directory -Force $dst | Out-Null
Copy-Item "$src\tesseract.exe" $dst
Copy-Item "$src\*.dll" $dst
Copy-Item "$src\*.txt" $dst -ErrorAction SilentlyContinue   # licence texts
& "$dst\tesseract.exe" --version
"AMWAPOS_TESSERACT=$((Resolve-Path "$dst\tesseract.exe").Path)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
