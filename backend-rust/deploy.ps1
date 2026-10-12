param([switch]$AllowUnconfiguredCloud)
$ErrorActionPreference = 'Stop'
$envPath = Join-Path $PSScriptRoot '.env'
function New-Secret([int]$Bytes) {
    $buffer = New-Object byte[] $Bytes
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($buffer) } finally { $rng.Dispose() }
    return ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant()
}
if (!(Test-Path $envPath)) {
    $content = Get-Content (Join-Path $PSScriptRoot '.env.example') -Raw
    $content = $content.Replace("MONGO_PASSWORD=", "MONGO_PASSWORD=$(New-Secret 24)")
    $content = $content.Replace("MONGO_APP_PASSWORD=", "MONGO_APP_PASSWORD=$(New-Secret 24)")
    $content = $content.Replace("JWT_SECRET=", "JWT_SECRET=$(New-Secret 48)")
    $content = $content.Replace("ADMIN_PASSWORD=", "ADMIN_PASSWORD=$(New-Secret 24)")
    [IO.File]::WriteAllText($envPath, $content, (New-Object Text.UTF8Encoding $false))
    Write-Host 'Created .env with independent random credentials. Administrator credentials are in that file.'
}
$values = @{}
Get-Content $envPath | ForEach-Object {
    if ($_ -match '^([A-Z_]+)=(.*)$') { $values[$matches[1]] = $matches[2].Trim().Trim('"') }
}
if (!$AllowUnconfiguredCloud) {
    foreach ($key in @('COS_REGION', 'COS_BUCKET', 'COS_SECRET_ID', 'COS_SECRET_KEY')) {
        if (!$values[$key]) { throw "Fill $key in $envPath and configure COS CORS before deployment. Use -AllowUnconfiguredCloud only for account/metadata testing; uploads will be unavailable." }
    }
}
& docker compose --env-file $envPath -f (Join-Path $PSScriptRoot 'compose.yaml') config --quiet
if ($LASTEXITCODE -ne 0) { throw 'Compose configuration validation failed' }
& docker compose --env-file $envPath -f (Join-Path $PSScriptRoot 'compose.yaml') up -d --build --wait --wait-timeout 180
if ($LASTEXITCODE -ne 0) { throw 'Deployment failed; inspect docker compose logs using the same env-file and compose path' }
Write-Host "Frontend: http://localhost:$($values['HTTP_PORT'])"
Write-Host "Captured local verification emails: http://localhost:$($values['MAIL_UI_PORT'])"
Write-Host 'Model operations require AI_BASE_URL, AI_API_KEY, and AI_MODEL. Business physical deletion stays gated until backup protection is confirmed.'
