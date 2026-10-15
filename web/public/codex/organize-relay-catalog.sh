#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export ORGANIZE_SCRIPT_DIR="$script_dir"

if ! command -v python3 >/dev/null 2>&1; then
  echo "Error: python3 is required for the shell version" >&2
  exit 1
fi

exec python3 - "$@" <<'PY'
import argparse
import copy
import json
import os
import re
import sys
import tempfile
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import Request, urlopen

try:
    import tomllib
except ModuleNotFoundError:
    try:
        import tomli as tomllib
    except ModuleNotFoundError as error:
        raise SystemExit(
            "Error: Python 3.11+ or the tomli package is required for TOML parsing"
        ) from error


SCRIPT_DIR = Path(os.environ["ORGANIZE_SCRIPT_DIR"]).resolve()
DEFAULT_CONFIG = (SCRIPT_DIR / ".." / "config.toml").resolve()
DEFAULT_CATALOG = (SCRIPT_DIR / "relay-mtwm81nx.json").resolve()
ALLOWED_MODALITIES = ("text", "image", "audio")

OBSOLETE_RULES = [
    re.compile(r"(?:^|/)gpt-5\.5$", re.IGNORECASE),
    re.compile(
        r"(?:^|/)deepseek-v4-(?:flash|flash-fast|flash-vision-exp)(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)kimi-k2\.(?:5|6)(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)glm-(?:4(?:\.\d+)?(?:v)?|5(?:-turbo|-v-turbo)?|5\.(?:1|2)(?:-fast)?)(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)minimax-m2(?:\.\d+)?(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)mimo-v2\.5(?:-(?:asr|tts|tts-voiceclone|tts-voicedesign|pro))?(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)qwen3\.(?:6|7)(?:-[a-z0-9]+)*(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)grok-4\.(?:5|6)(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)step-3\.(?:5|7)-flash(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?:^|/)hy3-paid(?:-(?:low|medium|high|xhigh|max))?$",
        re.IGNORECASE,
    ),
]

FIELD_ORDER = [
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
    "tool_mode",
]

REASONING_DESCRIPTIONS = {
    "none": "No reasoning",
    "low": "Fast responses with lighter reasoning",
    "medium": "Balances speed and reasoning depth for everyday tasks",
    "high": "Greater reasoning depth for complex problems",
    "xhigh": "Extra high reasoning depth for complex problems",
    "max": "Maximum reasoning depth for the hardest problems",
    "ultra": "Maximum reasoning with automatic task delegation",
}

KNOWN_CONTEXT_CAPS = [
    (
        re.compile(r"^callai/(?:codex-auto-review|gpt-6-(?:astra|luna|sol))$", re.IGNORECASE),
        400000,
    ),
]

BUILT_IN_FALLBACK = {
    "slug": "codex-fallback",
    "display_name": "Codex Fallback",
    "description": "Built-in fallback for remote models",
    "default_reasoning_level": "medium",
    "supported_reasoning_levels": [
        {"effort": "low", "description": REASONING_DESCRIPTIONS["low"]},
        {"effort": "medium", "description": REASONING_DESCRIPTIONS["medium"]},
        {"effort": "high", "description": REASONING_DESCRIPTIONS["high"]},
        {"effort": "xhigh", "description": REASONING_DESCRIPTIONS["xhigh"]},
    ],
    "shell_type": "shell_command",
    "visibility": "list",
    "supported_in_api": True,
    "priority": 1000,
    "base_instructions": (
        "You are Codex, a coding agent. Follow the user's instructions and "
        "repository conventions, inspect the relevant context before editing, "
        "make focused changes, and verify the result."
    ),
    "supports_reasoning_summaries": True,
    "default_reasoning_summary": "none",
    "support_verbosity": True,
    "default_verbosity": "low",
    "apply_patch_tool_type": "freeform",
    "web_search_tool_type": "text_and_image",
    "truncation_policy": {"mode": "tokens", "limit": 10000},
    "supports_parallel_tool_calls": True,
    "supports_image_detail_original": False,
    "effective_context_window_percent": 100,
    "experimental_supported_tools": [],
    "input_modalities": ["text"],
    "supports_search_tool": True,
    "use_responses_lite": False,
}


def positive_integer(value):
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be an integer >= 1")
    return parsed


def non_negative_integer(value):
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be an integer >= 0")
    return parsed


def shrink_percent(value):
    parsed = float(value)
    if parsed < 0 or parsed > 100:
        raise argparse.ArgumentTypeError("must be between 0 and 100")
    return parsed


def parse_args():
    parser = argparse.ArgumentParser(
        description="Organize a Codex relay model catalog from /v1/models."
    )
    parser.add_argument("--config", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--template", type=Path)
    parser.add_argument("--max-shrink-percent", type=shrink_percent, default=30.0)
    parser.add_argument("--timeout-ms", type=positive_integer, default=30000)
    parser.add_argument("--retries", type=non_negative_integer, default=2)
    parser.add_argument("--allow-empty", action="store_true")
    parser.add_argument("--allow-shrink", action="store_true")
    parser.add_argument("--allow-oauth-token", action="store_true")
    parser.add_argument("--no-template", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args()


def resolve_input_path(value):
    if value is None:
        return None
    return value.expanduser().resolve()


def read_json(path):
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise RuntimeError(f"Invalid JSON in {path}: {error}") from error


def parse_codex_config(config_path):
    try:
        config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        raise RuntimeError(f"Invalid TOML in {config_path}: {error}") from error

    provider_name = config.get("model_provider")
    if not isinstance(provider_name, str) or not provider_name:
        raise RuntimeError("config.toml does not define model_provider")

    providers = config.get("model_providers")
    if not isinstance(providers, dict) or provider_name not in providers:
        raise RuntimeError(f"config.toml does not define model_providers.{provider_name}")

    provider = providers[provider_name]
    if not isinstance(provider, dict):
        raise RuntimeError(f"model_providers.{provider_name} must be a table")
    base_url = provider.get("base_url")
    if not isinstance(base_url, str) or not base_url.strip():
        raise RuntimeError(f"model_providers.{provider_name} does not define base_url")

    env_key = provider.get("env_key")
    return {
        "model_provider": provider_name,
        "provider": {
            "base_url": base_url.strip(),
            "env_key": env_key.strip() if isinstance(env_key, str) and env_key.strip() else None,
            "requires_openai_auth": provider.get("requires_openai_auth") is True,
        },
    }


def build_models_url(base_url):
    parsed = urlsplit(base_url)
    if parsed.scheme not in ("http", "https"):
        raise RuntimeError(f"Unsupported base_url protocol: {parsed.scheme}")
    path = parsed.path.rstrip("/")
    if path.endswith("/models"):
        new_path = path
    elif path.endswith("/v1"):
        new_path = f"{path}/models"
    else:
        new_path = f"{path}/v1/models"
    return urlunsplit((parsed.scheme, parsed.netloc, new_path, "", ""))


def load_api_key(config_path, provider, allow_oauth_token):
    auth_path = config_path.parent / "auth.json"
    auth = read_json(auth_path) or {}

    env_key = provider["env_key"]
    if env_key:
        environment_value = os.environ.get(env_key)
        if environment_value:
            return environment_value
        stored_value = auth.get(env_key)
        if isinstance(stored_value, str) and stored_value:
            return stored_value
        raise RuntimeError(
            f"Provider requires {env_key}, but it is not present in the environment or {auth_path}"
        )

    environment_value = os.environ.get("OPENAI_API_KEY")
    if environment_value:
        return environment_value
    stored_value = auth.get("OPENAI_API_KEY")
    if isinstance(stored_value, str) and stored_value:
        return stored_value

    oauth_token = auth.get("tokens", {}).get("access_token")
    if isinstance(oauth_token, str) and oauth_token:
        if not provider["requires_openai_auth"]:
            raise RuntimeError(
                "Refusing to send auth.json tokens.access_token to an untrusted provider; "
                "configure env_key"
            )
        if not allow_oauth_token:
            raise RuntimeError(
                "Only an OAuth access token is available. Re-run with --allow-oauth-token "
                "after verifying the provider URL"
            )
        return oauth_token

    raise RuntimeError(f"No API key found in the environment or {auth_path}")


def fetch_json(url, api_key, timeout_ms, retries):
    maximum_attempts = retries + 1
    last_error = None

    for attempt in range(maximum_attempts):
        try:
            request = Request(
                url,
                headers={
                    "Accept": "application/json",
                    "Authorization": f"Bearer {api_key}",
                    "User-Agent": "organize-relay-catalog/2.0",
                },
            )
            with urlopen(request, timeout=timeout_ms / 1000.0) as response:
                body = response.read().decode("utf-8")
            return json.loads(body)
        except HTTPError as error:
            body = error.read().decode("utf-8", errors="replace")
            snippet = " ".join(body.split())[:500]
            last_error = RuntimeError(
                f"HTTP {error.code} from {url}{f': {snippet}' if snippet else ''}"
            )
            retryable = error.code == 429 or error.code >= 500
        except (URLError, TimeoutError, json.JSONDecodeError) as error:
            last_error = error
            retryable = True

        if not retryable or attempt == maximum_attempts - 1:
            break
        time.sleep(min(0.5 * (2**attempt), 5.0))

    raise RuntimeError(str(last_error))


def fetch_remote_models(config_path, options):
    config = parse_codex_config(config_path)
    models_url = build_models_url(config["provider"]["base_url"])
    api_key = load_api_key(config_path, config["provider"], options.allow_oauth_token)
    payload = fetch_json(models_url, api_key, options.timeout_ms, options.retries)

    if isinstance(payload.get("data"), list):
        raw_models = payload["data"]
    elif isinstance(payload.get("models"), list):
        raw_models = payload["models"]
    else:
        raise RuntimeError(f"{models_url} did not return a data or models array")

    models = []
    for remote in raw_models:
        if not isinstance(remote, dict):
            continue
        slug = remote.get("id") or remote.get("slug")
        if isinstance(slug, str) and slug:
            model = copy.deepcopy(remote)
            model["slug"] = slug
            models.append(model)

    if not models and not options.allow_empty:
        raise RuntimeError(f"{models_url} returned no usable models")

    return {
        "models": models,
        "models_url": models_url,
        "provider_name": config["model_provider"],
    }


def load_template(template_path):
    catalog = read_json(template_path)
    if catalog is None:
        return {"models": []}
    if not isinstance(catalog, dict) or not isinstance(catalog.get("models"), list):
        raise RuntimeError(f"Template {template_path} must contain a models array")
    return catalog


def get_fallback_template(template_catalog):
    models = template_catalog.get("models", [])
    for model in models:
        if model.get("slug") == "coding":
            return model
    for model in models:
        if model.get("base_instructions"):
            return model
    if models:
        return models[0]
    return copy.deepcopy(BUILT_IN_FALLBACK)


def normalize_modalities(values):
    if not isinstance(values, list):
        return ["text"]
    available = {value.lower() for value in values if isinstance(value, str)}
    normalized = [modality for modality in ALLOWED_MODALITIES if modality in available]
    return normalized or ["text"]


def normalize_efforts(values):
    if not isinstance(values, list):
        return []
    result = []
    seen = set()
    for value in values:
        if not isinstance(value, str):
            continue
        effort = value.strip().lower()
        if effort and effort not in seen:
            seen.add(effort)
            result.append(effort)
    return result


def set_reasoning_levels(model, efforts):
    unique = list(dict.fromkeys(efforts))
    model["supported_reasoning_levels"] = [
        {
            "effort": effort,
            "description": REASONING_DESCRIPTIONS.get(
                effort, f"{effort} reasoning effort"
            ),
        }
        for effort in unique
    ]
    if model.get("default_reasoning_level") not in unique:
        model["default_reasoning_level"] = (
            "medium" if "medium" in unique else unique[0]
        )


def merge_remote_model(remote, template, fallback, priority):
    has_template = template is not None
    model = copy.deepcopy(template if has_template else fallback)
    capabilities = remote.get("capabilities")
    if not isinstance(capabilities, dict):
        capabilities = None

    remote_name = remote.get("name") or remote.get("display_name") or remote["slug"]
    model["slug"] = remote["slug"]
    model["display_name"] = remote_name
    model["description"] = remote_name
    model["priority"] = priority

    context_values = [
        remote.get("context_length"),
        remote.get("max_input_tokens"),
        remote.get("max_context_window"),
        capabilities.get("context_limit") if capabilities else None,
        capabilities.get("input_limit") if capabilities else None,
        capabilities.get("max_context_window") if capabilities else None,
    ]
    for pattern, limit in KNOWN_CONTEXT_CAPS:
        if pattern.search(remote["slug"]):
            context_values.append(limit)

    context_limits = []
    for value in context_values:
        try:
            number = int(value)
        except (TypeError, ValueError):
            continue
        if number > 0:
            context_limits.append(number)

    if context_limits:
        context_window = min(context_limits)
        model["context_window"] = context_window
        model["max_context_window"] = context_window
        model["auto_compact_token_limit"] = context_window * 9 // 10
    elif not has_template:
        model.pop("context_window", None)
        model.pop("max_context_window", None)
        model.pop("auto_compact_token_limit", None)

    efforts = (
        normalize_efforts(remote.get("effort_tiers"))
        or normalize_efforts(capabilities.get("effort_tiers") if capabilities else None)
        or normalize_efforts(
            capabilities.get("reasoning_efforts") if capabilities else None
        )
    )
    if efforts:
        set_reasoning_levels(model, efforts)
    elif capabilities and capabilities.get("reasoning") is False:
        set_reasoning_levels(model, ["none"])
    elif not has_template:
        set_reasoning_levels(model, ["low", "medium", "high", "xhigh"])

    modality_values = None
    if capabilities and isinstance(capabilities.get("input_modalities"), list):
        modality_values = capabilities["input_modalities"]
    elif isinstance(remote.get("input_modalities"), list):
        modality_values = remote["input_modalities"]

    if modality_values is not None or capabilities is not None or not has_template:
        modalities = normalize_modalities(modality_values)
        model["input_modalities"] = modalities
        model["supports_image_detail_original"] = (
            "image" in modalities
            and capabilities is not None
            and capabilities.get("attachment") is not False
        ) if capabilities is not None else "image" in modalities

    return model


def is_obsolete_model(slug):
    return any(pattern.search(slug) for pattern in OBSOLETE_RULES)


def has_prompt_override(model):
    base_instructions = model.get("base_instructions")
    messages = model.get("model_messages")
    template = messages.get("instructions_template") if isinstance(messages, dict) else None
    has_base = isinstance(base_instructions, str) and bool(base_instructions.strip())
    has_messages = isinstance(template, str) and bool(template.strip())
    if not has_base and not has_messages:
        raise RuntimeError(f"Model {model.get('slug')} has no prompt source")
    return has_base


def clean_model(model):
    has_base = has_prompt_override(model)
    cleaned = {}
    for key in FIELD_ORDER:
        if key not in model:
            continue
        if key == "model_messages" and has_base:
            continue
        cleaned[key] = model[key]
    for key, value in model.items():
        if key in cleaned or key == "model_messages" or key in FIELD_ORDER:
            continue
        cleaned[key] = value
    return cleaned


def remove_duplicate_cmd_models(models):
    command_code_suffixes = {
        model["slug"][len("command-code/") :]
        for model in models
        if model["slug"].startswith("command-code/")
    }
    return [
        model
        for model in models
        if not model["slug"].startswith("cmd/")
        or model["slug"][len("cmd/") :] not in command_code_suffixes
    ]


def assert_unique_slugs(models):
    seen = set()
    duplicates = set()
    for model in models:
        slug = model["slug"]
        if slug in seen:
            duplicates.add(slug)
        seen.add(slug)
    if duplicates:
        raise RuntimeError(
            f"Remote catalog contains duplicate slugs: {', '.join(sorted(duplicates))}"
        )


def disambiguate_display_names(models):
    renamed = set()
    for _ in range(len(models)):
        counts = {}
        for model in models:
            name = model.get("display_name") or ""
            counts[name] = counts.get(name, 0) + 1
        duplicates = {name for name, count in counts.items() if count > 1}
        if not duplicates:
            return len(renamed)
        for model in models:
            if (model.get("display_name") or "") in duplicates:
                model["display_name"] = model["slug"]
                model["description"] = model["slug"]
                renamed.add(model["slug"])
    raise RuntimeError("Could not disambiguate all display names")


def organize_catalog(remote_models, template_catalog):
    templates = {model["slug"]: model for model in template_catalog.get("models", [])}
    fallback = get_fallback_template(template_catalog)
    current_models = []
    removed_obsolete = 0

    for model in remote_models:
        if is_obsolete_model(model["slug"]):
            removed_obsolete += 1
        else:
            current_models.append(model)

    deduplicated = remove_duplicate_cmd_models(current_models)
    assert_unique_slugs(deduplicated)
    removed_duplicate_cmd = len(current_models) - len(deduplicated)

    models = [
        clean_model(
            merge_remote_model(
                remote,
                templates.get(remote["slug"]),
                fallback,
                1000 + index,
            )
        )
        for index, remote in enumerate(deduplicated)
    ]
    disambiguated = disambiguate_display_names(models)

    catalog = copy.deepcopy(template_catalog)
    catalog["models"] = models
    summary = {
        "before": len(remote_models),
        "after": len(models),
        "previous": len(template_catalog.get("models", [])),
        "removedObsolete": removed_obsolete,
        "removedDuplicateCmd": removed_duplicate_cmd,
        "disambiguatedDisplayNames": disambiguated,
        "retainedOpenCodeGo": sum(
            model["slug"].startswith("opencode-go/") for model in models
        ),
        "newModelsWithoutTemplate": sum(
            model["slug"] not in templates for model in models
        ),
    }
    return catalog, summary


def validate_catalog_size(summary, options):
    if summary["after"] == 0 and not options.allow_empty:
        raise RuntimeError("Refusing to write an empty model catalog")
    if summary["previous"] == 0 or options.allow_shrink:
        return
    minimum = int(summary["previous"] * (1 - options.max_shrink_percent / 100.0))
    if summary["after"] < minimum:
        raise RuntimeError(
            f"Refusing to shrink catalog from {summary['previous']} to "
            f"{summary['after']} models; minimum is {minimum}. "
            "Use --allow-shrink to override."
        )


def atomic_write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        newline="\n",
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
        delete=False,
    ) as temporary:
        temporary.write(text)
        temporary_path = Path(temporary.name)
    try:
        os.replace(temporary_path, path)
    except BaseException:
        temporary_path.unlink(missing_ok=True)
        raise


def main():
    options = parse_args()
    config_path = resolve_input_path(options.config) or DEFAULT_CONFIG
    output_path = resolve_input_path(options.output) or DEFAULT_CATALOG
    template_path = (
        None
        if options.no_template
        else resolve_input_path(options.template) or output_path
    )

    template_catalog = load_template(template_path) if template_path else {"models": []}
    remote = fetch_remote_models(config_path, options)
    catalog, summary = organize_catalog(remote["models"], template_catalog)
    validate_catalog_size(summary, options)

    serialized = json.dumps(catalog, ensure_ascii=False, indent=2) + "\n"
    if not options.dry_run:
        atomic_write(output_path, serialized)

    result = {
        **summary,
        "configPath": str(config_path),
        "templatePath": str(template_path) if template_path else None,
        "outputPath": str(output_path),
        "provider": remote["provider_name"],
        "modelsUrl": remote["models_url"],
        "dryRun": options.dry_run,
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
PY
