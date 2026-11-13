# Install a Hanzo native binary on Windows (PowerShell).
#
#   irm https://raw.githubusercontent.com/hanzoai/cli/main/install.ps1 | iex
#
# Downloads <BIN>-windows-<arch>.tar.gz from GitHub Releases, verifies its
# .sha256 checksum, extracts <BIN>.exe into $HOME\.local\bin (or
# $env:HANZO_INSTALL_PREFIX), installs the alias copy, and ensures the directory
# is on the User PATH.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

function Invoke-HanzoInstall {
    param(
        [string]$Repo   = $(if ($env:HANZO_INSTALL_REPO)   { $env:HANZO_INSTALL_REPO }   else { 'hanzoai/cli' }),
        [string]$Bin    = $(if ($env:HANZO_INSTALL_BIN)    { $env:HANZO_INSTALL_BIN }    else { 'hanzo' }),
        [string]$Alias  = $(if (Test-Path env:HANZO_INSTALL_ALIAS) { $env:HANZO_INSTALL_ALIAS } else { 'hanzo-cli' }),
        [string]$Prefix = $(if ($env:HANZO_INSTALL_PREFIX) { $env:HANZO_INSTALL_PREFIX } else { Join-Path $HOME '.local\bin' }),
        [string]$Tag    = $(if ($env:HANZO_VERSION)        { $env:HANZO_VERSION }        else { '' })
    )

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13

    $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        'ARM64' { 'amd64' } # fallback if WoW64/x64 emulation; native amd64 asset
        default { 'amd64' }
    }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -and [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') {
        # Keep windows-amd64 as primary published target (x64 emulation supported on Windows 11 ARM64).
        $arch = 'amd64'
    }
    $target = "windows-$arch"
    $asset  = "$Bin-$target.tar.gz"

    $token = $env:HANZO_INSTALL_TOKEN
    if (-not $token) { $token = $env:GH_TOKEN }
    if (-not $token) { $token = $env:GITHUB_TOKEN }
    $dlHeaders  = @{ 'User-Agent' = 'hanzo-installer/1.0' }
    $apiHeaders = @{ 'User-Agent' = 'hanzo-installer/1.0' }
    if ($token) { $apiHeaders['Authorization'] = "Bearer $token" }

    function Get-TagCandidates {
        param([string]$RepoName, [hashtable]$Hdr)
        try {
            $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$RepoName/releases?per_page=30" -Headers $Hdr
            return @($rel | Where-Object { -not $_.draft } | ForEach-Object { $_.tag_name })
        } catch {
            # Fallback to git ls-remote when API rate limit hits anonymously.
            if (Get-Command git -ErrorAction SilentlyContinue) {
                $lines = git ls-remote --tags "https://github.com/$RepoName.git" 2>$null
                $items = foreach ($line in $lines) {
                    if ($line -match 'refs/tags/([^\^]+?)(?:\^\{\})?$') {
                        $tag = $Matches[1]
                        if ($tag -match '^(?:.*-)?v(\d+\.\d+\.\d+)$') {
                            [PSCustomObject]@{ Tag = $tag; Ver = [version]$Matches[1] }
                        }
                    }
                }
                return @($items | Sort-Object Ver -Descending | Select-Object -ExpandProperty Tag -Unique)
            }
            throw
        }
    }

    function Test-ReleaseAsset {
        param([string]$RepoName, [string]$ReleaseTag, [string]$AssetName, [hashtable]$Hdr)
        $shaUrl = "https://github.com/$RepoName/releases/download/$ReleaseTag/$AssetName.sha256"
        try {
            $resp = Invoke-WebRequest -UseBasicParsing -Method Head -Uri $shaUrl -Headers $Hdr -TimeoutSec 10
            return ($resp.StatusCode -ge 200 -and $resp.StatusCode -lt 400)
        } catch {
            return $false
        }
    }

    if (-not $Tag) {
        $candidates = Get-TagCandidates -RepoName $Repo -Hdr $apiHeaders
        foreach ($cand in $candidates) {
            if (Test-ReleaseAsset -RepoName $Repo -ReleaseTag $cand -AssetName $asset -Hdr $dlHeaders) {
                $Tag = $cand
                break
            }
        }
        if (-not $Tag) {
            throw "${Bin}: no stable release of $Repo has a published $target build."
        }
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("hanzo-inst-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $base = "https://github.com/$Repo/releases/download/$Tag"
        $tgzPath = Join-Path $tmp $asset
        $shaPath = Join-Path $tmp "$asset.sha256"

        Write-Host "${Bin}: $Tag $target"
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -Headers $dlHeaders -OutFile $tgzPath
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -Headers $dlHeaders -OutFile $shaPath

        $want = ((Get-Content -Raw $shaPath) -split '\s+')[0].Trim().ToLowerInvariant()
        if ($want.Length -ne 64) {
            throw "${Bin}: invalid checksum file for $asset"
        }
        $got = (Get-FileHash -Algorithm SHA256 $tgzPath).Hash.ToLowerInvariant()
        if ($got -ne $want) {
            throw "${Bin}: checksum MISMATCH for $asset (want $want, got $got)"
        }

        $tarExe = (Get-Command tar.exe -ErrorAction SilentlyContinue).Source
        if (-not $tarExe) { $tarExe = (Get-Command tar -ErrorAction Stop).Source }
        & $tarExe -xzf $tgzPath -C $tmp
        if ($LASTEXITCODE -ne 0) {
            throw "${Bin}: failed extracting $asset"
        }

        $extracted = Get-ChildItem -Path $tmp -Filter "$Bin.exe" -Recurse | Select-Object -First 1
        if (-not $extracted) {
            throw "${Bin}: archive $asset did not contain $Bin.exe"
        }

        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        $dest = Join-Path $Prefix "$Bin.exe"
        $old  = "$dest.old"
        if (Test-Path $old) { Remove-Item -Force $old -ErrorAction SilentlyContinue }
        if (Test-Path $dest) { Move-Item -Force $dest $old -ErrorAction SilentlyContinue }
        Move-Item -Force $extracted.FullName $dest
        if (Test-Path $old) { Remove-Item -Force $old -ErrorAction SilentlyContinue }

        Write-Host "${Bin}: installed $dest"

        if ($Alias -and $Alias -ne $Bin) {
            $aliasDest = Join-Path $Prefix "$Alias.exe"
            $aliasOld  = "$aliasDest.old"
            if (Test-Path $aliasOld) { Remove-Item -Force $aliasOld -ErrorAction SilentlyContinue }
            if (Test-Path $aliasDest) { Move-Item -Force $aliasDest $aliasOld -ErrorAction SilentlyContinue }
            Copy-Item -Force $dest $aliasDest
            if (Test-Path $aliasOld) { Remove-Item -Force $aliasOld -ErrorAction SilentlyContinue }
            Write-Host "${Bin}: installed $aliasDest (alias)"
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    # Ensure $Prefix is on User PATH and current session PATH.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @()
    if ($userPath) { $parts = $userPath -split ';' | Where-Object { $_ } }
    if ($parts -notcontains $Prefix) {
        $newPath = (@($Prefix) + $parts) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "${Bin}: added $Prefix to User PATH"
    }
    if (($env:Path -split ';') -notcontains $Prefix) {
        $env:Path = "$Prefix;$env:Path"
    }

    if ($Bin -eq 'hanzo') {
        Write-Host "hanzo: next -> hanzo auth login"
    }
}

Invoke-HanzoInstall
