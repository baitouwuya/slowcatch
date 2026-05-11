param(
    [string]$Version,
    [string]$InstallDir = (Join-Path $env:USERPROFILE ".codex\bin"),
    [string]$CodexHome = (Join-Path $env:USERPROFILE ".codex"),
    [switch]$Force,
    [string]$AssetPath
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

function Read-Utf8TextNoBom {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $null
    }

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -eq 0) {
        return ""
    }

    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    return $text.TrimStart([char]0xFEFF)
}

function Write-Utf8NoBom {
    param(
        [string]$Path,
        [string]$Text
    )

    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Backup-InvalidHooksJson {
    param([string]$HooksPath)

    if (-not (Test-Path -LiteralPath $HooksPath)) {
        return $null
    }

    $timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $backupPath = "$HooksPath.bak.$timestamp"
    Copy-Item -LiteralPath $HooksPath -Destination $backupPath -Force
    return $backupPath
}

function Test-SlowcatchHookCommand {
    param($Hook)

    if ($null -eq $Hook -or -not ($Hook.Contains("command"))) {
        return $false
    }

    $command = [string]$Hook["command"]
    return $command -match '(?i)(slowcatch|rust-fast-tool)(?:\.exe)?["'']?\s+hook\s+codex'
}

function ConvertTo-ArrayValue {
    param($Value)

    if ($null -eq $Value) {
        return @()
    }

    if ($Value -is [System.Array]) {
        return @($Value)
    }

    return @($Value)
}

function Normalize-HookGroups {
    param($Groups)

    $normalized = @()
    foreach ($group in (ConvertTo-ArrayValue $Groups)) {
        if ($null -eq $group) {
            continue
        }

        if (-not ($group -is [System.Collections.IDictionary])) {
            $normalized += ,$group
            continue
        }

        if ($group.Contains("hooks")) {
            $group["hooks"] = @(ConvertTo-ArrayValue $group["hooks"])
        } else {
            $group["hooks"] = @()
        }

        $normalized += ,$group
    }

    return $normalized
}

function Assert-CodexHooksSchema {
    param($Document)

    if ($null -eq $Document -or -not ($Document -is [System.Collections.IDictionary])) {
        throw "Invalid hooks config: document must be an object."
    }

    if (-not $Document.Contains("hooks")) {
        return
    }

    $hooks = $Document["hooks"]
    if ($null -eq $hooks) {
        return
    }

    if (-not ($hooks -is [System.Collections.IDictionary])) {
        throw "Invalid hooks config: hooks must be an object."
    }

    foreach ($event in $hooks.Keys) {
        if (-not ($hooks[$event] -is [System.Array])) {
            throw "Invalid hooks config: hooks.$event must be an array."
        }

        foreach ($group in $hooks[$event]) {
            if ($group -is [System.Collections.IDictionary] -and
                $group.Contains("hooks") -and
                -not ($group["hooks"] -is [System.Array])) {
                throw "Invalid hooks config: hooks.$event[].hooks must be an array."
            }
        }
    }
}

function Normalize-AllHookEvents {
    param($Hooks)

    if ($null -eq $Hooks -or -not ($Hooks -is [System.Collections.IDictionary])) {
        return
    }

    foreach ($eventName in @($Hooks.Keys)) {
        $Hooks[$eventName] = @(Normalize-HookGroups $Hooks[$eventName])
    }
}

function New-HookCommand {
    param([string]$ExePath)

    if ($ExePath -match '\s') {
        $escaped = $ExePath.Replace('"', '\"')
        return "& `"$escaped`" hook codex"
    }

    return "$ExePath hook codex"
}

function Remove-SlowcatchHooksFromGroups {
    param($Groups)

    $mergedGroups = @()
    foreach ($group in (Normalize-HookGroups $Groups)) {
        if ($null -eq $group -or -not ($group -is [System.Collections.IDictionary])) {
            $mergedGroups += ,$group
            continue
        }

        $originalHooks = @(ConvertTo-ArrayValue $group["hooks"])
        $group["hooks"] = $originalHooks

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

    return $mergedGroups
}

function Merge-CodexHook {
    param(
        [string]$HooksPath,
        [string]$ExePath
    )

    if (Test-Path -LiteralPath $HooksPath) {
        $raw = Read-Utf8TextNoBom $HooksPath
        if ([string]::IsNullOrWhiteSpace($raw)) {
            $document = [ordered]@{}
        } else {
            try {
                $document = ConvertTo-Hashtable ($raw | ConvertFrom-Json)
            } catch {
                $backupPath = Backup-InvalidHooksJson $HooksPath
                Write-Warning "Existing hooks.json is not valid JSON. Backed it up to $backupPath and created a fresh hooks.json."
                $document = [ordered]@{}
            }
        }
    } else {
        $document = [ordered]@{}
    }

    if (-not $document.Contains("hooks") -or $null -eq $document["hooks"]) {
        $document["hooks"] = [ordered]@{}
    }

    $hooks = $document["hooks"]
    Normalize-AllHookEvents $hooks

    $preToolUse = if ($hooks.Contains("PreToolUse")) { @(Normalize-HookGroups $hooks["PreToolUse"]) } else { @() }
    $userPromptSubmit = if ($hooks.Contains("UserPromptSubmit")) { @(Normalize-HookGroups $hooks["UserPromptSubmit"]) } else { @() }
    $postToolUse = if ($hooks.Contains("PostToolUse")) { @(Normalize-HookGroups $hooks["PostToolUse"]) } else { @() }
    $stop = if ($hooks.Contains("Stop")) { @(Normalize-HookGroups $hooks["Stop"]) } else { @() }

    $mergedGroups = @(Remove-SlowcatchHooksFromGroups -Groups $preToolUse | Where-Object { $null -ne $_ })

    $mergedGroups += ,([ordered]@{
        matcher = "^Bash$"
        hooks = @(
            [ordered]@{
                type = "command"
                command = (New-HookCommand $ExePath)
                timeout = 15
                statusMessage = "Checking slowcatch fast-path"
            }
        )
    })

    $hooks["PreToolUse"] = $mergedGroups

    $promptGroups = @(Remove-SlowcatchHooksFromGroups -Groups $userPromptSubmit | Where-Object { $null -ne $_ })
    $promptGroups += ,([ordered]@{
        hooks = @(
            [ordered]@{
                type = "command"
                command = (New-HookCommand $ExePath)
                timeout = 10
                statusMessage = "Looking up prompt references"
            }
        )
    })

    $hooks["UserPromptSubmit"] = $promptGroups

    $postGroups = @(Remove-SlowcatchHooksFromGroups -Groups $postToolUse | Where-Object { $null -ne $_ })
    $postGroups += ,([ordered]@{
        matcher = "^apply_patch$|^Edit$|^Write$"
        hooks = @(
            [ordered]@{
                type = "command"
                command = (New-HookCommand $ExePath)
                timeout = 10
                statusMessage = "Checking edited files"
            }
        )
    })

    $hooks["PostToolUse"] = $postGroups

    $stopGroups = @(Remove-SlowcatchHooksFromGroups -Groups $stop | Where-Object { $null -ne $_ })
    $stopGroups += ,([ordered]@{
        hooks = @(
            [ordered]@{
                type = "command"
                command = (New-HookCommand $ExePath)
                timeout = 25
                statusMessage = "Checking project diagnostics"
            }
        )
    })

    $hooks["Stop"] = $stopGroups

    Normalize-AllHookEvents $hooks

    Assert-CodexHooksSchema $document

    $parent = Split-Path -Parent $HooksPath
    if (-not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    $json = ($document | ConvertTo-Json -Depth 32) + [Environment]::NewLine
    Write-Utf8NoBom -Path $HooksPath -Text $json
}

function Test-CodexHooksFeatureEnabled {
    param([string]$ConfigPath)

    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        return $false
    }

    $content = Read-Utf8TextNoBom $ConfigPath
    return $content -match '(?m)^\s*codex_hooks\s*=\s*true\s*(#.*)?$'
}

function Install-SlowcatchBinary {
    param(
        [string]$SourcePath,
        [string]$TargetExe,
        [string]$ReleaseVersion
    )

    $safeVersion = $ReleaseVersion -replace '[^A-Za-z0-9._-]', '-'
    $timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $candidates = @(
        $TargetExe,
        (Join-Path (Split-Path -Parent $TargetExe) "slowcatch-$safeVersion.exe"),
        (Join-Path (Split-Path -Parent $TargetExe) "slowcatch-$safeVersion-$timestamp.exe")
    )

    $lastError = $null
    foreach ($candidate in $candidates) {
        try {
            [System.IO.File]::Copy($SourcePath, $candidate, $true)
            if ($candidate -ne $TargetExe) {
                Write-Warning "Could not overwrite $TargetExe. Installing versioned binary instead: $candidate"
            }
            return $candidate
        } catch {
            $lastError = $_
        }
    }

    throw $lastError
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
$sourcePath = $null
try {
    if (-not [string]::IsNullOrWhiteSpace($AssetPath)) {
        $sourcePath = [System.IO.Path]::GetFullPath($AssetPath)
        if (-not (Test-Path -LiteralPath $sourcePath)) {
            throw "AssetPath does not exist: $sourcePath"
        }
    } else {
        Invoke-WebRequest -Uri $downloadUrl -OutFile $tempFile -UseBasicParsing -Headers @{ "User-Agent" = $UserAgent }
        $sourcePath = $tempFile
    }

    if ((Test-Path -LiteralPath $targetExe) -and $Force) {
        Remove-Item -LiteralPath $targetExe -Force
    }

    $installedExe = Install-SlowcatchBinary -SourcePath $sourcePath -TargetExe $targetExe -ReleaseVersion $releaseVersion
} finally {
    if (Test-Path -LiteralPath $tempFile) {
        Remove-Item -LiteralPath $tempFile -Force
    }
}

Merge-CodexHook -HooksPath $hooksPath -ExePath $installedExe

Write-Host "Updated Codex hooks: $hooksPath"
Write-Host "Hook command uses: $installedExe"

if (-not (Test-CodexHooksFeatureEnabled $configPath)) {
    Write-Warning "Codex hooks may not be enabled. Add this to ${configPath}:"
    Write-Host ""
    Write-Host "[features]"
    Write-Host "codex_hooks = true"
    Write-Host ""
}

Write-Host "Done. Restart Codex or reload hooks if your current session does not pick up hook changes automatically."
