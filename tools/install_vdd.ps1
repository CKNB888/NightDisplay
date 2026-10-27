# Source-tree compatibility entry point.
& (Join-Path $PSScriptRoot '../nightdisplay/resources/install_vdd.ps1') @args
exit $LASTEXITCODE
