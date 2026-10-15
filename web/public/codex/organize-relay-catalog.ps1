#!/usr/bin/env pwsh

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$defaultConfig = [IO.Path]::GetFullPath((Join-Path $scriptDir "..\config.toml"))
$defaultCatalog = Join-Path $scriptDir "relay-mtwm81nx.json"
$allowedModalities = @("text", "image", "audio")

$obsoleteRules = @(
  '(?:^|/)gpt-5\.5$',
  '(?:^|/)deepseek-v4-(?:flash|flash-fast|flash-vision-exp)(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)kimi-k2\.(?:5|6)(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)glm-(?:4(?:\.\d+)?(?:v)?|5(?:-turbo|-v-turbo)?|5\.(?:1|2)(?:-fast)?)(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)minimax-m2(?:\.\d+)?(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)mimo-v2\.5(?:-(?:asr|tts|tts-voiceclone|tts-voicedesign|pro))?(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)qwen3\.(?:6|7)(?:-[a-z0-9]+)*(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)grok-4\.(?:5|6)(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)step-3\.(?:5|7)-flash(?:-(?:low|medium|high|xhigh|max))?$',
  '(?:^|/)hy3-paid(?:-(?:low|medium|high|xhigh|max))?$'
)

$fieldOrder = @(
  "slug",
  "display_name",
  "description",
  "default_reasoning_level",
  "supported_reasoning_levels",
  "shell_type",
  "visibility",
  "supported_in_api",
  "priority",
  "additional_speed_tiers",
  "service_tiers",
  "availability_nux",
  "upgrade",
  "base_instructions",
  "model_messages",
  "supports_reasoning_summaries",
  "default_reasoning_summary",
  "support_verbosity",
  "default_verbosity",
  "apply_patch_tool_type",
  "web_search_tool_type",
  "truncation_policy",
  "supports_parallel_tool_calls",
  "supports_image_detail_original",
  "context_window",
  "max_context_window",
  "effective_context_window_percent",
  "experimental_supported_tools",
  "input_modalities",
  "supports_search_tool",
  "auto_compact_token_limit",
  "use_responses_lite",
  "multi_agent_version",
  "tool_mode"
)

$reasoningDescriptions = @{
  none = "No reasoning"
  low = "Fast responses with lighter reasoning"
  medium = "Balances speed and reasoning depth for everyday tasks"
  high = "Greater reasoning depth for complex problems"
  xhigh = "Extra high reasoning depth for complex problems"
  max = "Maximum reasoning depth for the hardest problems"
  ultra = "Maximum reasoning with automatic task delegation"
}

$knownContextCaps = @(
  @{
    model = '^callai/(?:codex-auto-review|gpt-6-(?:astra|luna|sol))$'
    limit = 400000
  }
)

$builtInFallbackTemplate = [pscustomobject]@{
  slug = "codex-fallback"
  display_name = "Codex Fallback"
  description = "Built-in fallback for remote models"
  default_reasoning_level = "medium"
  supported_reasoning_levels = @(
    [pscustomobject]@{ effort = "low"; description = $reasoningDescriptions.low }
    [pscustomobject]@{ effort = "medium"; description = $reasoningDescriptions.medium }
    [pscustomobject]@{ effort = "high"; description = $reasoningDescriptions.high }
    [pscustomobject]@{ effort = "xhigh"; description = $reasoningDescriptions.xhigh }
  )
  shell_type = "shell_command"
  visibility = "list"
  supported_in_api = $true
  priority = 1000
  base_instructions = "You are Codex, a coding agent. Follow the user's instructions and repository conventions, inspect the relevant context before editing, make focused changes, and verify the result."
  supports_reasoning_summaries = $true
  default_reasoning_summary = "none"
  support_verbosity = $true
  default_verbosity = "low"
  apply_patch_tool_type = "freeform"
  web_search_tool_type = "text_and_image"
  truncation_policy = [pscustomobject]@{
    mode = "tokens"
    limit = 10000
  }
  supports_parallel_tool_calls = $true
  supports_image_detail_original = $false
  effective_context_window_percent = 100
  experimental_supported_tools = @()
  input_modalities = @("text")
  supports_search_tool = $true
  use_responses_lite = $false
}

function Write-Help {
  @'
Usage: pwsh ./organize-relay-catalog.ps1 [options]

Options:
  --config <path>              Codex config.toml path (default: ../config.toml)
  --output <path>              Output catalog path (default: relay-mtwm81nx.json)
  --template <path>            Existing catalog used for prompts and defaults
  --max-shrink-percent <n>     Refuse a catalog shrink larger than n% (default: 30)
  --timeout-ms <n>             Per-request timeout in milliseconds (default: 30000)
  --retries <n>                Retries for network and 429/5xx errors (default: 2)
  --allow-empty                Allow writing an empty model catalog
  --allow-shrink               Allow a shrink larger than max-shrink-percent
  --allow-oauth-token          Allow sending auth.json tokens.access_token
  --no-template                Force the built-in fallback instead of using a template
  --dry-run                    Fetch and validate without writing the catalog
  --help                       Show this help
'@ | Write-Output
}

function Get-OptionValue {
  param(
    [string[]]$Arguments,
    [int]$Index,
    [string]$Name,
    [string]$InlineValue,
    [bool]$InlineSpecified
  )

  if ($InlineSpecified) {
    if (-not $InlineValue) { throw "$Name requires a value" }
    return @{ Value = $InlineValue; Consumed = 0 }
  }

  $next = if ($Index + 1 -lt $Arguments.Count) { $Arguments[$Index + 1] } else { $null }
  if (-not $next -or $next.StartsWith("--")) {
    throw "$Name requires a value"
  }
  return @{ Value = $next; Consumed = 1 }
}

function Parse-Options {
  param([string[]]$Arguments)

  $options = @{
    ConfigPath = $null
    OutputPath = $null
    TemplatePath = $null
    MaxShrinkPercent = 30.0
    TimeoutMs = 30000
    Retries = 2
    AllowEmpty = $false
    AllowShrink = $false
    AllowOAuthToken = $false
    NoTemplate = $false
    DryRun = $false
    Help = $false
  }

  for ($index = 0; $index -lt $Arguments.Count; $index += 1) {
    $token = $Arguments[$index]
    $equals = $token.IndexOf("=")
    $name = if ($equals -eq -1) { $token } else { $token.Substring(0, $equals) }
    $inline = if ($equals -eq -1) { $null } else { $token.Substring($equals + 1) }

    switch ($name) {
      "--config" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $options.ConfigPath = $result.Value
        $index += $result.Consumed
      }
      "--output" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $options.OutputPath = $result.Value
        $index += $result.Consumed
      }
      "--template" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $options.TemplatePath = $result.Value
        $index += $result.Consumed
      }
      "--max-shrink-percent" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $value = 0.0
        if (-not [double]::TryParse($result.Value, [ref]$value) -or $value -lt 0 -or $value -gt 100) {
          throw "$name must be a number between 0 and 100"
        }
        $options.MaxShrinkPercent = $value
        $index += $result.Consumed
      }
      "--timeout-ms" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $value = 0
        if (-not [int]::TryParse($result.Value, [ref]$value) -or $value -lt 1) {
          throw "$name must be an integer >= 1"
        }
        $options.TimeoutMs = $value
        $index += $result.Consumed
      }
      "--retries" {
        $result = Get-OptionValue -Arguments $Arguments -Index $index -Name $name -InlineValue $inline -InlineSpecified ($equals -ne -1)
        $value = -1
        if (-not [int]::TryParse($result.Value, [ref]$value) -or $value -lt 0) {
          throw "$name must be an integer >= 0"
        }
        $options.Retries = $value
        $index += $result.Consumed
      }
      "--allow-empty" { $options.AllowEmpty = $true }
      "--allow-shrink" { $options.AllowShrink = $true }
      "--allow-oauth-token" { $options.AllowOAuthToken = $true }
      "--no-template" { $options.NoTemplate = $true }
      "--dry-run" { $options.DryRun = $true }
      "--help" { $options.Help = $true }
      "-h" { $options.Help = $true }
      default { throw "Unknown option: $token" }
    }
  }

  return $options
}

function Resolve-InputPath {
  param([string]$Path)

  if ([IO.Path]::IsPathRooted($Path)) {
    return [IO.Path]::GetFullPath($Path)
  }
  return [IO.Path]::GetFullPath((Join-Path (Get-Location).Path $Path))
}

function Remove-TomlComment {
  param([string]$Line)

  $quote = [char]0
  $escaped = $false
  for ($index = 0; $index -lt $Line.Length; $index += 1) {
    $character = $Line[$index]
    if ($escaped) {
      $escaped = $false
      continue
    }
    if ($character -eq '\' -and $quote -eq '"') {
      $escaped = $true
      continue
    }
    if (($character -eq '"' -or $character -eq "'") -and ($quote -eq [char]0 -or $quote -eq $character)) {
      $quote = if ($quote -eq [char]0) { $character } else { [char]0 }
      continue
    }
    if ($character -eq '#' -and $quote -eq [char]0) {
      return $Line.Substring(0, $index)
    }
  }

  return $Line
}

function Test-UnclosedContainer {
  param([string]$Text)

  $quote = [char]0
  $escaped = $false
  $squareDepth = 0
  $braceDepth = 0

  foreach ($character in $Text.ToCharArray()) {
    if ($escaped) {
      $escaped = $false
      continue
    }
    if ($character -eq '\' -and $quote -eq '"') {
      $escaped = $true
      continue
    }
    if (($character -eq '"' -or $character -eq "'") -and ($quote -eq [char]0 -or $quote -eq $character)) {
      $quote = if ($quote -eq [char]0) { $character } else { [char]0 }
      continue
    }
    if ($quote -ne [char]0) { continue }
    if ($character -eq '[') { $squareDepth += 1 }
    if ($character -eq ']') { $squareDepth -= 1 }
    if ($character -eq '{') { $braceDepth += 1 }
    if ($character -eq '}') { $braceDepth -= 1 }
  }

  return $quote -ne [char]0 -or $squareDepth -gt 0 -or $braceDepth -gt 0
}

function Get-TomlStatements {
  param([string]$Text)

  $statements = New-Object System.Collections.Generic.List[string]
  $pending = ""

  foreach ($rawLine in [regex]::Split($Text, "\r?\n")) {
    if ($rawLine.Contains('"""') -or $rawLine.Contains("'''")) { continue }
    $line = Remove-TomlComment $rawLine
    if (-not $line.Trim() -and -not $pending) { continue }
    $pending = if ($pending) { "$pending`n$line" } else { $line }
    if (-not (Test-UnclosedContainer $pending)) {
      $statements.Add($pending)
      $pending = ""
    }
  }

  if ($pending.Trim()) { $statements.Add($pending) }
  return $statements
}

function Find-TopLevelEquals {
  param([string]$Text)

  $quote = [char]0
  $escaped = $false
  $squareDepth = 0
  $braceDepth = 0

  for ($index = 0; $index -lt $Text.Length; $index += 1) {
    $character = $Text[$index]
    if ($escaped) {
      $escaped = $false
      continue
    }
    if ($character -eq '\' -and $quote -eq '"') {
      $escaped = $true
      continue
    }
    if (($character -eq '"' -or $character -eq "'") -and ($quote -eq [char]0 -or $quote -eq $character)) {
      $quote = if ($quote -eq [char]0) { $character } else { [char]0 }
      continue
    }
    if ($quote -ne [char]0) { continue }
    if ($character -eq '[') { $squareDepth += 1 }
    if ($character -eq ']') { $squareDepth -= 1 }
    if ($character -eq '{') { $braceDepth += 1 }
    if ($character -eq '}') { $braceDepth -= 1 }
    if ($character -eq '=' -and $squareDepth -eq 0 -and $braceDepth -eq 0) {
      return $index
    }
  }

  return -1
}

function ConvertFrom-TomlKeySegment {
  param([string]$Segment)

  $trimmed = $Segment.Trim()
  if ($trimmed.StartsWith('"') -and $trimmed.EndsWith('"')) {
    try {
      return ConvertFrom-Json $trimmed
    } catch {
      throw "Invalid quoted TOML key: $Segment"
    }
  }
  if ($trimmed.StartsWith("'") -and $trimmed.EndsWith("'")) {
    return $trimmed.Substring(1, $trimmed.Length - 2)
  }
  if ($trimmed -match '^[A-Za-z0-9_-]+$') { return $trimmed }
  throw "Invalid TOML key segment: $Segment"
}

function Get-TomlKeySegments {
  param([string]$Text)

  $segments = New-Object System.Collections.Generic.List[string]
  $cursor = 0
  while ($cursor -lt $Text.Length) {
    while ($cursor -lt $Text.Length -and [char]::IsWhiteSpace($Text[$cursor])) { $cursor += 1 }
    if ($cursor -ge $Text.Length) { break }

    if ($Text[$cursor] -eq '"' -or $Text[$cursor] -eq "'") {
      $quote = $Text[$cursor]
      $start = $cursor
      $cursor += 1
      $closed = $false
      while ($cursor -lt $Text.Length) {
        if ($quote -eq '"' -and $Text[$cursor] -eq '\') {
          $cursor += 2
          continue
        }
        if ($Text[$cursor] -eq $quote) {
          $closed = $true
          $cursor += 1
          break
        }
        $cursor += 1
      }
      if (-not $closed) { throw "Invalid TOML key '$Text': unterminated quoted segment" }
      $segments.Add((ConvertFrom-TomlKeySegment $Text.Substring($start, $cursor - $start)))
    } else {
      $start = $cursor
      while ($cursor -lt $Text.Length -and $Text[$cursor] -match '[A-Za-z0-9_-]') { $cursor += 1 }
      if ($cursor -eq $start) { throw "Invalid TOML key '$Text'" }
      $segments.Add($Text.Substring($start, $cursor - $start))
    }

    while ($cursor -lt $Text.Length -and [char]::IsWhiteSpace($Text[$cursor])) { $cursor += 1 }
    if ($cursor -ge $Text.Length) { break }
    if ($Text[$cursor] -ne ".") { throw "Invalid TOML key '$Text'" }
    $cursor += 1
  }

  return $segments
}

function ConvertFrom-TomlScalar {
  param([string]$Text)

  $trimmed = $Text.Trim()
  if ($trimmed.StartsWith('"') -and $trimmed.EndsWith('"')) {
    try {
      return ConvertFrom-Json $trimmed
    } catch {
      throw "Invalid TOML basic string: $Text"
    }
  }
  if ($trimmed.StartsWith("'") -and $trimmed.EndsWith("'")) {
    return $trimmed.Substring(1, $trimmed.Length - 2)
  }
  if ($trimmed -eq "true") { return $true }
  if ($trimmed -eq "false") { return $false }
  return $trimmed
}

function Split-TopLevelComma {
  param([string]$Text)

  $parts = New-Object System.Collections.Generic.List[string]
  $quote = [char]0
  $escaped = $false
  $squareDepth = 0
  $braceDepth = 0
  $start = 0

  for ($index = 0; $index -lt $Text.Length; $index += 1) {
    $character = $Text[$index]
    if ($escaped) {
      $escaped = $false
      continue
    }
    if ($character -eq '\' -and $quote -eq '"') {
      $escaped = $true
      continue
    }
    if (($character -eq '"' -or $character -eq "'") -and ($quote -eq [char]0 -or $quote -eq $character)) {
      $quote = if ($quote -eq [char]0) { $character } else { [char]0 }
      continue
    }
    if ($quote -ne [char]0) { continue }
    if ($character -eq '[') { $squareDepth += 1 }
    if ($character -eq ']') { $squareDepth -= 1 }
    if ($character -eq '{') { $braceDepth += 1 }
    if ($character -eq '}') { $braceDepth -= 1 }
    if ($character -eq ',' -and $squareDepth -eq 0 -and $braceDepth -eq 0) {
      $parts.Add($Text.Substring($start, $index - $start))
      $start = $index + 1
    }
  }

  $parts.Add($Text.Substring($start))
  return $parts
}

function ConvertFrom-TomlInlineTable {
  param([string]$Text)

  $trimmed = $Text.Trim()
  if (-not $trimmed.StartsWith("{") -or -not $trimmed.EndsWith("}")) {
    throw "Invalid TOML inline table: $Text"
  }

  $result = @{}
  $body = $trimmed.Substring(1, $trimmed.Length - 2).Trim()
  if (-not $body) { return $result }

  foreach ($entry in (Split-TopLevelComma $body)) {
    if (-not $entry.Trim()) { continue }
    $equals = Find-TopLevelEquals $entry
    if ($equals -eq -1) { throw "Invalid TOML inline table entry: $entry" }
    $key = Get-TomlKeySegments $entry.Substring(0, $equals)
    $rawValue = $entry.Substring($equals + 1).Trim()
    $value = if ($rawValue.StartsWith("{")) {
      ConvertFrom-TomlInlineTable $rawValue
    } else {
      ConvertFrom-TomlScalar $rawValue
    }
    Set-TomlPath -Target $result -Segments $key -Value $value
  }

  return $result
}

function Set-TomlPath {
  param(
    [hashtable]$Target,
    [string[]]$Segments,
    $Value
  )

  if ($Segments.Count -eq 0) { throw "TOML key cannot be empty" }
  $current = $Target
  for ($index = 0; $index -lt $Segments.Count - 1; $index += 1) {
    $segment = $Segments[$index]
    if (-not $current.ContainsKey($segment) -or $current[$segment] -isnot [hashtable]) {
      $current[$segment] = @{}
    }
    $current = $current[$segment]
  }
  $current[$Segments[$Segments.Count - 1]] = $Value
}

function ConvertFrom-CodexToml {
  param([string]$Text)

  $root = @{}
  $section = @()

  foreach ($statement in (Get-TomlStatements $Text)) {
    $trimmed = $statement.Trim()
    if (-not $trimmed) { continue }

    if ($trimmed.StartsWith("[")) {
      if (-not $trimmed.EndsWith("]")) { throw "Invalid TOML section: $trimmed" }
      $section = @(Get-TomlKeySegments $trimmed.Substring(1, $trimmed.Length - 2))
      continue
    }

    $equals = Find-TopLevelEquals $trimmed
    if ($equals -eq -1) { continue }
    $key = @(Get-TomlKeySegments $trimmed.Substring(0, $equals))
    $fullPath = @($section + $key)
    if ($fullPath.Count -eq 0) { continue }
    if ($fullPath[0] -notin @("model_provider", "model_providers")) { continue }

    $rawValue = $trimmed.Substring($equals + 1).Trim()
    $value = if ($rawValue.StartsWith("{")) {
      ConvertFrom-TomlInlineTable $rawValue
    } else {
      ConvertFrom-TomlScalar $rawValue
    }
    Set-TomlPath -Target $root -Segments $fullPath -Value $value
  }

  $providerName = if ($root.ContainsKey("model_provider")) { $root.model_provider } else { $null }
  if (-not $providerName -or $providerName -isnot [string]) {
    throw "config.toml does not define model_provider"
  }
  if (-not $root.ContainsKey("model_providers") -or $root.model_providers -isnot [hashtable]) {
    throw "config.toml does not define model_providers"
  }
  if (-not $root.model_providers.ContainsKey($providerName)) {
    throw "config.toml does not define model_providers.$providerName"
  }

  $provider = $root.model_providers[$providerName]
  if ($provider -isnot [hashtable] -or -not $provider.base_url) {
    throw "model_providers.$providerName does not define base_url"
  }

  return @{
    ModelProvider = $providerName
    Provider = @{
      BaseUrl = [string]$provider.base_url
      EnvKey = if ($provider.env_key) { [string]$provider.env_key } else { $null }
      RequiresOpenAIAuth = $provider.requires_openai_auth -eq $true
    }
  }
}

function Build-ModelsUrl {
  param([string]$BaseUrl)

  $builder = [System.UriBuilder]::new($BaseUrl)
  if ($builder.Scheme -notin @("http", "https")) {
    throw "Unsupported base_url protocol: $($builder.Scheme)"
  }

  $path = $builder.Path.TrimEnd("/")
  if ($path.EndsWith("/models")) {
    $builder.Path = $path
  } elseif ($path.EndsWith("/v1")) {
    $builder.Path = "$path/models"
  } else {
    $builder.Path = "$path/v1/models"
  }
  $builder.Query = ""
  $builder.Fragment = ""
  return $builder.Uri
}

function Read-JsonFile {
  param([string]$Path)

  if (-not [IO.File]::Exists($Path)) { return $null }
  try {
    return [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) | ConvertFrom-Json
  } catch {
    throw "Invalid JSON in ${Path}: $($_.Exception.Message)"
  }
}

function Get-EnvironmentValue {
  param([string]$Name)
  return [Environment]::GetEnvironmentVariable($Name)
}

function Get-ApiKey {
  param(
    [string]$ConfigPath,
    [hashtable]$Provider,
    [bool]$AllowOAuthToken
  )

  $authPath = Join-Path (Split-Path -Parent $ConfigPath) "auth.json"
  $auth = Read-JsonFile $authPath

  if ($Provider.EnvKey) {
    $environmentValue = Get-EnvironmentValue $Provider.EnvKey
    if ($environmentValue) { return $environmentValue }
    if ($auth -and $auth.PSObject.Properties.Name -contains $Provider.EnvKey) {
      $stored = $auth.PSObject.Properties[$Provider.EnvKey].Value
      if ($stored) { return [string]$stored }
    }
    throw "Provider requires $($Provider.EnvKey), but it is not present in the environment or $authPath"
  }

  $openAiEnvironment = Get-EnvironmentValue "OPENAI_API_KEY"
  if ($openAiEnvironment) { return $openAiEnvironment }
  if ($auth -and $auth.PSObject.Properties.Name -contains "OPENAI_API_KEY" -and $auth.OPENAI_API_KEY) {
    return [string]$auth.OPENAI_API_KEY
  }

  $oauthToken = $null
  if ($auth -and $auth.tokens -and $auth.tokens.PSObject.Properties.Name -contains "access_token") {
    $oauthToken = $auth.tokens.access_token
  }
  if ($oauthToken) {
    if (-not $Provider.RequiresOpenAIAuth) {
      throw "Refusing to send auth.json tokens.access_token to an untrusted provider; configure env_key"
    }
    if (-not $AllowOAuthToken) {
      throw "Only an OAuth access token is available. Re-run with --allow-oauth-token after verifying the provider URL"
    }
    return [string]$oauthToken
  }

  throw "No API key found in the environment or $authPath"
}

function Invoke-JsonRequest {
  param(
    [uri]$Uri,
    [string]$ApiKey,
    [int]$TimeoutMs,
    [int]$Retries
  )

  $headers = @{
    Accept = "application/json"
    Authorization = "Bearer $ApiKey"
  }
  $timeoutSeconds = [Math]::Max(1, [Math]::Ceiling($TimeoutMs / 1000.0))
  $maximumAttempts = $Retries + 1
  $lastError = $null

  for ($attempt = 0; $attempt -lt $maximumAttempts; $attempt += 1) {
    try {
      return Invoke-RestMethod -Uri $Uri.AbsoluteUri -Headers $headers -Method Get -TimeoutSec $timeoutSeconds
    } catch {
      $lastError = $_
      $statusCode = 0
      if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
        $statusCode = [int]$_.Exception.Response.StatusCode
      }
      $retryable = $statusCode -eq 429 -or $statusCode -ge 500 -or $statusCode -eq 0
      if (-not $retryable -or $attempt -eq $maximumAttempts - 1) { break }
      $delay = [Math]::Min(500 * [Math]::Pow(2, $attempt), 5000)
      Start-Sleep -Milliseconds ([int]$delay)
    }
  }

  $details = if ($lastError.ErrorDetails.Message) { $lastError.ErrorDetails.Message } else {
    $lastError.Exception.Message
  }
  throw "Failed to fetch ${Uri}: $details"
}

function Fetch-RemoteModels {
  param(
    [string]$ConfigPath,
    [hashtable]$Options
  )

  $config = ConvertFrom-CodexToml ([IO.File]::ReadAllText($ConfigPath, [Text.Encoding]::UTF8))
  $modelsUrl = Build-ModelsUrl $config.Provider.BaseUrl
  $apiKey = Get-ApiKey -ConfigPath $ConfigPath -Provider $config.Provider -AllowOAuthToken $Options.AllowOAuthToken
  $payload = Invoke-JsonRequest -Uri $modelsUrl -ApiKey $apiKey -TimeoutMs $Options.TimeoutMs -Retries $Options.Retries

  $rawModels = if ($payload.data) { @($payload.data) } elseif ($payload.models) { @($payload.models) } else { $null }
  if ($null -eq $rawModels) {
    throw "$modelsUrl did not return a data or models array"
  }

  $models = @(
    foreach ($model in $rawModels) {
      $slug = if ($model.id) { $model.id } elseif ($model.slug) { $model.slug } else { $null }
      if ($slug -is [string] -and $slug) {
        $copy = Copy-Deep $model
        Set-PropertyValue -Object $copy -Name "slug" -Value $slug
        $copy
      }
    }
  )

  if ($models.Count -eq 0 -and -not $Options.AllowEmpty) {
    throw "$modelsUrl returned no usable models"
  }

  return @{
    Models = $models
    ModelsUrl = $modelsUrl.AbsoluteUri
    ProviderName = $config.ModelProvider
  }
}

function Copy-Deep {
  param($Value)

  if ($null -eq $Value) { return $null }
  if ($Value -is [string]) { return $Value }
  if ($Value -is [System.Collections.IDictionary]) {
    $copy = @{}
    foreach ($key in $Value.Keys) { $copy[$key] = Copy-Deep $Value[$key] }
    return $copy
  }
  if ($Value -is [System.Collections.IEnumerable]) {
    $items = New-Object System.Collections.ArrayList
    foreach ($item in $Value) { [void]$items.Add((Copy-Deep $item)) }
    return ,$items.ToArray()
  }
  if ($Value -is [pscustomobject]) {
    $copy = [pscustomobject]@{}
    foreach ($property in $Value.PSObject.Properties) {
      $copy | Add-Member -NotePropertyName $property.Name -NotePropertyValue (Copy-Deep $property.Value)
    }
    return $copy
  }
  return $Value
}

function Test-Property {
  param($Object, [string]$Name)
  return $null -ne $Object -and $Object.PSObject.Properties.Name -contains $Name
}

function Get-PropertyValue {
  param($Object, [string]$Name, $Default = $null)
  if (Test-Property $Object $Name) {
    return ,$Object.PSObject.Properties[$Name].Value
  }
  return $Default
}

function Set-PropertyValue {
  param($Object, [string]$Name, $Value)
  if (Test-Property $Object $Name) {
    $Object.PSObject.Properties[$Name].Value = $Value
  } else {
    $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
  }
}

function Remove-PropertyValue {
  param($Object, [string]$Name)
  if (Test-Property $Object $Name) {
    $Object.PSObject.Properties.Remove($Name)
  }
}

function Load-Template {
  param([string]$TemplatePath)

  if (-not [IO.File]::Exists($TemplatePath)) { return [pscustomobject]@{ models = @() } }
  $catalog = Read-JsonFile $TemplatePath
  if (-not $catalog -or -not (Test-Property $catalog "models")) {
    throw "Template $TemplatePath must contain a models array"
  }
  return $catalog
}

function Get-FallbackTemplate {
  param($TemplateCatalog)

  $models = @($TemplateCatalog.models)
  $fallback = $models | Where-Object { $_.slug -eq "coding" } | Select-Object -First 1
  if (-not $fallback) {
    $fallback = $models | Where-Object { $_.base_instructions } | Select-Object -First 1
  }
  if (-not $fallback -and $models.Count -gt 0) { $fallback = $models[0] }
  if (-not $fallback) { return Copy-Deep $builtInFallbackTemplate }
  return $fallback
}

function Convert-ToPositiveNumber {
  param($Value)

  if ($null -eq $Value) { return $null }
  $number = 0.0
  if ([double]::TryParse([string]$Value, [ref]$number) -and $number -gt 0) { return $number }
  return $null
}

function Set-ReasoningLevels {
  param($Model, [string[]]$Efforts)

  $unique = @($Efforts | Where-Object { $_ } | Select-Object -Unique)
  $levels = @(
    foreach ($effort in $unique) {
      $description = if ($reasoningDescriptions.ContainsKey($effort)) {
        $reasoningDescriptions[$effort]
      } else {
        "$effort reasoning effort"
      }
      [pscustomobject]@{ effort = $effort; description = $description }
    }
  )
  Set-PropertyValue -Object $Model -Name "supported_reasoning_levels" -Value $levels

  $default = Get-PropertyValue $Model "default_reasoning_level" $null
  if ($unique -notcontains $default) {
    $default = if ($unique -contains "medium") { "medium" } else { $unique[0] }
    Set-PropertyValue -Object $Model -Name "default_reasoning_level" -Value $default
  }
}

function Normalize-Modalities {
  param($Values)

  $available = @{}
  foreach ($value in @($Values)) {
    if ($value -is [string] -and $value) { $available[$value.ToLowerInvariant()] = $true }
  }
  $normalized = @($allowedModalities | Where-Object { $available.ContainsKey($_) })
  if ($normalized.Count -eq 0) { return @("text") }
  return $normalized
}

function Merge-RemoteModel {
  param(
    $RemoteModel,
    $Template,
    $FallbackTemplate,
    [int]$Priority
  )

  $hasTemplate = $null -ne $Template
  $model = Copy-Deep $(if ($hasTemplate) { $Template } else { $FallbackTemplate })
  $capabilities = Get-PropertyValue $RemoteModel "capabilities" $null
  if ($capabilities -isnot [pscustomobject]) { $capabilities = $null }

  $remoteName = Get-PropertyValue $RemoteModel "name" $null
  if (-not $remoteName) { $remoteName = Get-PropertyValue $RemoteModel "display_name" $null }
  if (-not $remoteName) { $remoteName = $RemoteModel.slug }

  Set-PropertyValue $model "slug" $RemoteModel.slug
  Set-PropertyValue $model "display_name" $remoteName
  Set-PropertyValue $model "description" $remoteName
  Set-PropertyValue $model "priority" $Priority

  $capContextLimit = $null
  $capInputLimit = $null
  $capMaxContextWindow = $null
  $capEffortTiers = $null
  $capReasoningEfforts = $null
  if ($capabilities) {
    $capContextLimit = Get-PropertyValue $capabilities "context_limit" $null
    $capInputLimit = Get-PropertyValue $capabilities "input_limit" $null
    $capMaxContextWindow = Get-PropertyValue $capabilities "max_context_window" $null
    $capEffortTiers = Get-PropertyValue $capabilities "effort_tiers" $null
    $capReasoningEfforts = Get-PropertyValue $capabilities "reasoning_efforts" $null
  }

  $limits = New-Object System.Collections.Generic.List[double]
  foreach ($value in @(
      (Get-PropertyValue $RemoteModel "context_length" $null),
      (Get-PropertyValue $RemoteModel "max_input_tokens" $null),
      (Get-PropertyValue $RemoteModel "max_context_window" $null),
      $capContextLimit,
      $capInputLimit,
      $capMaxContextWindow
    )) {
    $number = Convert-ToPositiveNumber $value
    if ($null -ne $number) { $limits.Add($number) }
  }
  foreach ($cap in $knownContextCaps) {
    if ($RemoteModel.slug -match $cap.model) { $limits.Add([double]$cap.limit) }
  }

  if ($limits.Count -gt 0) {
    $contextWindow = ($limits | Measure-Object -Minimum).Minimum
    Set-PropertyValue $model "context_window" $contextWindow
    Set-PropertyValue $model "max_context_window" $contextWindow
    Set-PropertyValue $model "auto_compact_token_limit" ([Math]::Floor($contextWindow * 0.9))
  } elseif (-not $hasTemplate) {
    Remove-PropertyValue $model "context_window"
    Remove-PropertyValue $model "max_context_window"
    Remove-PropertyValue $model "auto_compact_token_limit"
  }

  $efforts = @()
  foreach ($candidate in @(
      (Get-PropertyValue $RemoteModel "effort_tiers" $null),
      $capEffortTiers,
      $capReasoningEfforts
    )) {
    if ($candidate -is [System.Collections.IEnumerable] -and $candidate -isnot [string]) {
      foreach ($effort in $candidate) {
        if ($effort -is [string] -and $effort.Trim()) {
          $efforts += $effort.Trim().ToLowerInvariant()
        }
      }
    }
  }
  $efforts = @($efforts | Select-Object -Unique)

  $reasoning = if ($capabilities) { Get-PropertyValue $capabilities "reasoning" $null } else { $null }
  if ($efforts.Count -gt 0) {
    Set-ReasoningLevels $model $efforts
  } elseif ($reasoning -eq $false) {
    Set-ReasoningLevels $model @("none")
  } elseif (-not $hasTemplate) {
    Set-ReasoningLevels $model @("low", "medium", "high", "xhigh")
  }

  $modalityValues = $null
  if ($capabilities -and (Test-Property $capabilities "input_modalities")) {
    $modalityValues = @($capabilities.input_modalities)
  } elseif (Test-Property $RemoteModel "input_modalities") {
    $modalityValues = @($RemoteModel.input_modalities)
  }

  if ($null -ne $modalityValues -or $capabilities -or -not $hasTemplate) {
    $modalities = @(Normalize-Modalities $modalityValues)
    Set-PropertyValue $model "input_modalities" $modalities
    $supportsOriginal = $modalities -contains "image"
    if ($capabilities -and (Get-PropertyValue $capabilities "attachment" $null) -eq $false) {
      $supportsOriginal = $false
    }
    if ($capabilities -and (Get-PropertyValue $capabilities "vision" $null) -eq $false) {
      $supportsOriginal = $false
    }
    Set-PropertyValue $model "supports_image_detail_original" $supportsOriginal
  }

  return $model
}

function Test-ObsoleteModel {
  param([string]$Slug)

  foreach ($rule in $obsoleteRules) {
    if ($Slug -match $rule) { return $true }
  }
  return $false
}

function Test-HasPromptOverride {
  param($Model)

  $baseInstructions = Get-PropertyValue $Model "base_instructions" ""
  $hasBaseInstructions = $baseInstructions -is [string] -and $baseInstructions.Trim()
  $hasMessages = $false
  $messages = Get-PropertyValue $Model "model_messages" $null
  if ($messages) {
    $template = Get-PropertyValue $messages "instructions_template" ""
    $hasMessages = $template -is [string] -and $template.Trim()
  }
  if (-not $hasBaseInstructions -and -not $hasMessages) {
    throw "Model $($Model.slug) has no prompt source"
  }
  return $hasBaseInstructions
}

function Clean-Model {
  param($Model)

  $hasBaseInstructions = Test-HasPromptOverride $Model
  $cleaned = [ordered]@{}

  foreach ($key in $fieldOrder) {
    if (-not (Test-Property $Model $key)) { continue }
    if ($key -eq "model_messages" -and $hasBaseInstructions) { continue }
    $cleaned[$key] = Get-PropertyValue $Model $key $null
  }

  foreach ($property in $Model.PSObject.Properties) {
    if ($cleaned.Contains($property.Name) -or $property.Name -eq "model_messages" -or $fieldOrder -contains $property.Name) {
      continue
    }
    $cleaned[$property.Name] = $property.Value
  }

  return [pscustomobject]$cleaned
}

function Remove-DuplicateCmdModels {
  param($Models)

  $suffixes = @{}
  foreach ($model in $Models) {
    if ($model.slug.StartsWith("command-code/")) {
      $suffixes[$model.slug.Substring("command-code/".Length)] = $true
    }
  }

  return @(
    foreach ($model in $Models) {
      if (-not $model.slug.StartsWith("cmd/")) {
        $model
      } elseif (-not $suffixes.ContainsKey($model.slug.Substring("cmd/".Length))) {
        $model
      }
    }
  )
}

function Assert-UniqueSlugs {
  param($Models)

  $seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
  $duplicates = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
  foreach ($model in $Models) {
    if (-not $seen.Add($model.slug)) { [void]$duplicates.Add($model.slug) }
  }
  if ($duplicates.Count -gt 0) {
    throw "Remote catalog contains duplicate slugs: $([string]::Join(', ', $duplicates))"
  }
}

function Disambiguate-DisplayNames {
  param($Models)

  $renamed = @{}
  for ($pass = 0; $pass -lt $Models.Count; $pass += 1) {
    $counts = @{}
    foreach ($model in $Models) {
      $name = if ($model.display_name) { $model.display_name } else { "" }
      $counts[$name] = 1 + $(if ($counts.ContainsKey($name)) { $counts[$name] } else { 0 })
    }
    $duplicates = @($counts.Keys | Where-Object { $counts[$_] -gt 1 })
    if ($duplicates.Count -eq 0) { return $renamed.Count }

    foreach ($model in $Models) {
      $name = if ($model.display_name) { $model.display_name } else { "" }
      if ($duplicates -contains $name) {
        Set-PropertyValue $model "display_name" $model.slug
        Set-PropertyValue $model "description" $model.slug
        $renamed[$model.slug] = $true
      }
    }
  }

  throw "Could not disambiguate all display names"
}

function Organize-Catalog {
  param($RemoteModels, $TemplateCatalog)

  $templates = @{}
  foreach ($model in @($TemplateCatalog.models)) { $templates[$model.slug] = $model }
  $fallback = Get-FallbackTemplate $TemplateCatalog

  $currentModels = New-Object System.Collections.ArrayList
  $removedObsolete = 0
  foreach ($model in $RemoteModels) {
    if (Test-ObsoleteModel $model.slug) {
      $removedObsolete += 1
    } else {
      [void]$currentModels.Add($model)
    }
  }

  $deduplicated = @(Remove-DuplicateCmdModels @($currentModels.ToArray()))
  Assert-UniqueSlugs $deduplicated
  $removedDuplicateCmd = $currentModels.Count - $deduplicated.Count

  $models = New-Object System.Collections.ArrayList
  for ($index = 0; $index -lt $deduplicated.Count; $index += 1) {
    $remoteModel = $deduplicated[$index]
    $template = if ($templates.ContainsKey($remoteModel.slug)) { $templates[$remoteModel.slug] } else { $null }
    $merged = Merge-RemoteModel -RemoteModel $remoteModel -Template $template -FallbackTemplate $fallback -Priority (1000 + $index)
    [void]$models.Add((Clean-Model $merged))
  }

  $modelArray = @($models.ToArray())
  $disambiguated = Disambiguate-DisplayNames $modelArray
  $catalog = Copy-Deep $TemplateCatalog
  Set-PropertyValue $catalog "models" $modelArray

  $newModels = 0
  $openCodeGo = 0
  foreach ($model in $modelArray) {
    if (-not $templates.ContainsKey($model.slug)) { $newModels += 1 }
    if ($model.slug.StartsWith("opencode-go/")) { $openCodeGo += 1 }
  }

  return @{
    Catalog = $catalog
    Summary = [ordered]@{
      before = $RemoteModels.Count
      after = $modelArray.Count
      previous = @($TemplateCatalog.models).Count
      removedObsolete = $removedObsolete
      removedDuplicateCmd = $removedDuplicateCmd
      disambiguatedDisplayNames = $disambiguated
      retainedOpenCodeGo = $openCodeGo
      newModelsWithoutTemplate = $newModels
    }
  }
}

function Assert-CatalogSize {
  param([hashtable]$Summary, [hashtable]$Options)

  if ($Summary.after -eq 0 -and -not $Options.AllowEmpty) {
    throw "Refusing to write an empty model catalog"
  }
  if ($Summary.previous -eq 0 -or $Options.AllowShrink) { return }
  $minimum = [Math]::Floor($Summary.previous * (1 - $Options.MaxShrinkPercent / 100.0))
  if ($Summary.after -lt $minimum) {
    throw "Refusing to shrink catalog from $($Summary.previous) to $($Summary.after) models; minimum is $minimum. Use --allow-shrink to override."
  }
}

function Write-AtomicText {
  param([string]$Path, [string]$Text)

  $directory = [IO.Path]::GetDirectoryName($Path)
  [IO.Directory]::CreateDirectory($directory) | Out-Null
  $temporary = Join-Path $directory ".$([IO.Path]::GetFileName($Path)).$PID.$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()).tmp"
  $backup = "$temporary.previous"
  $utf8 = [Text.UTF8Encoding]::new($false)

  try {
    [IO.File]::WriteAllText($temporary, $Text, $utf8)
    if ([IO.File]::Exists($Path)) {
      [IO.File]::Replace($temporary, $Path, $backup, $true)
      if ([IO.File]::Exists($backup)) { [IO.File]::Delete($backup) }
    } else {
      [IO.File]::Move($temporary, $Path)
    }
  } catch {
    if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    if ([IO.File]::Exists($backup) -and -not [IO.File]::Exists($Path)) {
      [IO.File]::Move($backup, $Path)
    }
    throw
  }
}

$options = Parse-Options -Arguments $args
if ($options.Help) {
  Write-Help
  exit 0
}

$configPath = if ($options.ConfigPath) {
  Resolve-InputPath $options.ConfigPath
} else {
  $defaultConfig
}
$outputPath = if ($options.OutputPath) {
  Resolve-InputPath $options.OutputPath
} else {
  $defaultCatalog
}
$templatePath = if ($options.NoTemplate) {
  $null
} elseif ($options.TemplatePath) {
  Resolve-InputPath $options.TemplatePath
} else {
  $outputPath
}

$templateCatalog = if ($templatePath) { Load-Template $templatePath } else { [pscustomobject]@{ models = @() } }
$remote = Fetch-RemoteModels -ConfigPath $configPath -Options $options
$organized = Organize-Catalog -RemoteModels $remote.Models -TemplateCatalog $templateCatalog
Assert-CatalogSize -Summary $organized.Summary -Options $options

$json = ConvertTo-Json -InputObject $organized.Catalog -Depth 100
$json = $json + [Environment]::NewLine
if (-not $options.DryRun) {
  Write-AtomicText -Path $outputPath -Text $json
}

$result = [ordered]@{}
foreach ($key in $organized.Summary.Keys) { $result[$key] = $organized.Summary[$key] }
$result.configPath = $configPath
$result.templatePath = $templatePath
$result.outputPath = $outputPath
$result.provider = $remote.ProviderName
$result.modelsUrl = $remote.ModelsUrl
$result.dryRun = $options.DryRun

ConvertTo-Json -InputObject $result -Depth 10
