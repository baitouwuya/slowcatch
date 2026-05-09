param(
    [string]$Version,
    [string]$InstallDir = (Join-Path $env:USERPROFILE ".codex\bin"),
    [string]$CodexHome = (Join-Path $env:USERPROFILE ".codex"),
    [switch]$Force
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$Repo = "baitouwuya/slowcatch"
$AssetName = "slowcatch-x86_64-pc-windows-msvc.exe"
$UserAgent = "slowcatch-installer"

function Invoke-GitHubJson {
    param([string]$Uri)

    $response = Invoke-WebRequest -Uri $Uri -UseBasicParsing -Headers @{ "User-Agent" = $UserAgent }
    return $response.Content | ConvertFrom-Json
}

function Resolve-ReleaseVersion {
    if (-not [string]::IsNullOrWhiteSpace($Version)) {
        return $Version
    }

    $release = Invoke-GitHubJson "https://api.github.com/repos/$Repo/releases/latest"
    if ([string]::IsNullOrWhiteSpace($release.tag_name)) {
        throw "Could not resolve latest release for $Repo."
    }
    return [string]$release.tag_name
}

function ConvertTo-Hashtable {
    param([Parameter(ValueFromPipeline = $true)]$InputObject)

    process {
        if ($null -eq $InputObject) {
            return $null
        }

        if ($InputObject -is [System.Collections.IDictionary]) {
            $result = [ordered]@{}
            foreach ($key in $InputObject.Keys) {
                $result[$key] = ConvertTo-Hashtable $InputObject[$key]
            }
            return $result
        }

        if ($InputObject -is [System.Collections.IEnumerable] -and $InputObject -isnot [string]) {
            $items = @()
            foreach ($item in $InputObject) {
                $items += ,(ConvertTo-Hashtable $item)
            }
            return $items
        }

        if ($InputObject -is [pscustomobject]) {
            $result = [ordered]@{}
            foreach ($property in $InputObject.PSObject.Properties) {
                $result[$property.Name] = ConvertTo-Hashtable $property.Value
            }
            return $result
        }

        return $InputObject
    }
}

function Test-SlowcatchHookCommand {
    param($Hook)

    if ($null -eq $Hook -or -not ($Hook.Contains("command"))) {
        return $false
    }

    $command = [string]$Hook["command"]
    return $command -match '(?i)(slowcatch|rust-fast-tool)(?:\.exe)?["'']?\s+hook\s+codex'
}

function New-HookCommand {
    param([string]$ExePath)

    if ($ExePath -match '\s') {
        $escaped = $ExePath.Replace('"', '\"')
        return "& `"$escaped`" hook codex"
    }

    return "$ExePath hook codex"
}

function Merge-CodexHook {
    param(
        [string]$HooksPath,
        [string]$ExePath
    )

    if (Test-Path -LiteralPath $HooksPath) {
        $raw = Get-Content -LiteralPath $HooksPath -Raw
        if ([string]::IsNullOrWhiteSpace($raw)) {
            $document = [ordered]@{}
        } else {
            $document = ConvertTo-Hashtable ($raw | ConvertFrom-Json)
        }
    } else {
        $document = [ordered]@{}
    }

    if (-not $document.Contains("hooks") -or $null -eq $document["hooks"]) {
        $document["hooks"] = [ordered]@{}
    }

    $hooks = $document["hooks"]
    if (-not $hooks.Contains("PreToolUse") -or $null -eq $hooks["PreToolUse"]) {
        $preToolUse = @()
    } else {
        $preToolUse = @($hooks["PreToolUse"])
    }

    $mergedGroups = @()
    foreach ($group in $preToolUse) {
        if ($null -eq $group -or -not ($group -is [System.Collections.IDictionary])) {
            $mergedGroups += ,$group
            continue
        }

        $originalHooks = @()
        if ($group.Contains("hooks") -and $null -ne $group["hooks"]) {
            $originalHooks = @($group["hooks"])
        }

        $remainingHooks = @()
        $removedRustHook = $false
        foreach ($hook in $originalHooks) {
            if ($hook -is [System.Collections.IDictionary] -and (Test-SlowcatchHookCommand $hook)) {
                $removedRustHook = $true
                continue
            }
            $remainingHooks += ,$hook
        }

        if ($removedRustHook) {
            if ($remainingHooks.Count -gt 0) {
                $group["hooks"] = $remainingHooks
                $mergedGroups += ,$group
            }
        } else {
            $mergedGroups += ,$group
        }
    }

    $mergedGroups += ,[ordered]@{
        matcher = "^Bash$"
        hooks = @(
            [ordered]@{
                type = "command"
                command = New-HookCommand $ExePath
                timeout = 10
                statusMessage = "Checking slowcatch fast-path"
            }
        )
    }

    $hooks["PreToolUse"] = $mergedGroups

    $parent = Split-Path -Parent $HooksPath
    if (-not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    $json = $document | ConvertTo-Json -Depth 32
    Set-Content -LiteralPath $HooksPath -Value $json -Encoding UTF8
}

function Test-CodexHooksFeatureEnabled {
    param([string]$ConfigPath)

    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        return $false
    }

    $content = Get-Content -LiteralPath $ConfigPath -Raw
    return $content -match '(?m)^\s*codex_hooks\s*=\s*true\s*(#.*)?$'
}

$releaseVersion = Resolve-ReleaseVersion
$downloadUrl = "https://github.com/$Repo/releases/download/$releaseVersion/$AssetName"
$targetDir = [System.IO.Path]::GetFullPath($InstallDir)
$codexHomeDir = [System.IO.Path]::GetFullPath($CodexHome)
$targetExe = Join-Path $targetDir "slowcatch.exe"
$hooksPath = Join-Path $codexHomeDir "hooks.json"
$configPath = Join-Path $codexHomeDir "config.toml"

Write-Host "Installing slowcatch $releaseVersion"
Write-Host "Download: $downloadUrl"
Write-Host "Target:   $targetExe"

if (-not (Test-Path -LiteralPath $targetDir)) {
    New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
}

$tempFile = Join-Path ([System.IO.Path]::GetTempPath()) ("slowcatch-" + [System.Guid]::NewGuid().ToString("N") + ".exe")
try {
    Invoke-WebRequest -Uri $downloadUrl -OutFile $tempFile -UseBasicParsing -Headers @{ "User-Agent" = $UserAgent }

    if ((Test-Path -LiteralPath $targetExe) -and $Force) {
        Remove-Item -LiteralPath $targetExe -Force
    }

    Move-Item -LiteralPath $tempFile -Destination $targetExe -Force
} finally {
    if (Test-Path -LiteralPath $tempFile) {
        Remove-Item -LiteralPath $tempFile -Force
    }
}

Merge-CodexHook -HooksPath $hooksPath -ExePath $targetExe

Write-Host "Updated Codex hooks: $hooksPath"

if (-not (Test-CodexHooksFeatureEnabled $configPath)) {
    Write-Warning "Codex hooks may not be enabled. Add this to ${configPath}:"
    Write-Host ""
    Write-Host "[features]"
    Write-Host "codex_hooks = true"
    Write-Host ""
}

Write-Host "Done. Restart Codex or reload hooks if your current session does not pick up hook changes automatically."
