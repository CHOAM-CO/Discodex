#!/usr/bin/env python3
"""Generate source-free installer tables from the canonical product resources."""

import argparse
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PRODUCT = ROOT / "resources/product.json"
MESSAGES = ROOT / "resources/messages"


def load_resources():
    product = json.loads(PRODUCT.read_text(encoding="utf-8"))
    messages = {}
    if MESSAGES.exists():
        for path in sorted(MESSAGES.glob("*.json")):
            messages.update(json.loads(path.read_text(encoding="utf-8")))
    return product, messages


def shell_quote(value):
    return "'" + value.replace("'", "'\"'\"'") + "'"


def powershell_quote(value):
    return "'" + value.replace("'", "''") + "'"


def generated_shell(product, messages):
    metadata = product["product"]
    lines = [
        "# Generated from resources/product.json and resources/messages/*.json.",
        "# Regenerate with python scripts/product_resources.py generate.",
        f"DISCODEX_PRODUCT_NAME={shell_quote(metadata['name'])}",
        f"DISCODEX_DATA_DIRECTORY={shell_quote(metadata['dataDirectory'])}",
        f"DISCODEX_RELEASE_DOWNLOAD_ROOT={shell_quote(metadata['releaseDownloadRoot'].rstrip('/'))}",
        f"DISCODEX_CONFIG_FILE={shell_quote(metadata['configFile'])}",
        f"DISCODEX_OWNERSHIP_FILE={shell_quote(metadata['ownershipFile'])}",
        f"DISCODEX_INSTALL_LIST_FILE={shell_quote(metadata['installationListFile'])}",
        f"DISCODEX_CREDENTIALS_FILE={shell_quote(metadata['credentialsFile'])}",
        f"DISCODEX_STATE_DIRECTORY={shell_quote(metadata['stateDirectory'])}",
        f"DISCODEX_BINARY_DIRECTORY={shell_quote(metadata['binaryDirectory'])}",
        f"DISCODEX_MESSAGE_ACCESS_FILE={shell_quote(metadata['messageAccessFile'])}",
        f"DISCODEX_DISCORD_COMMAND_FILE={shell_quote(metadata['discordCommandFile'])}",
        f"DISCODEX_RUNTIME_DIRECTORY={shell_quote(metadata['runtimeDirectory'])}",
        f"DISCODEX_UPDATES_FILE={shell_quote(metadata['updatesFile'])}",
        f"DISCODEX_SUPPORTED_LANGUAGES={shell_quote(' '.join(metadata['supportedLanguages']))}",
        f"DISCODEX_INSTALL_TEMP_PREFIX={shell_quote(metadata['installTempTemplate'].replace('{id}', ''))}",
        f"DISCODEX_UPDATE_TEMP_PREFIX={shell_quote(metadata['updateTempTemplate'].replace('{id}', ''))}",
        f"DISCODEX_INSTALL_STAGING_PREFIX={shell_quote(metadata['installStagingTemplate'].replace('{id}', ''))}",
    ]
    lines += [
        "discodex_platform_target() {",
        "  case \"$1:$2\" in",
    ]
    for platform in product["platforms"]:
        if platform["shellOs"] is None:
            continue
        for arch in platform["shellArch"]:
            lines.append(
                f"    {platform['shellOs']}:{arch}) printf '%s' {shell_quote(platform['target'])} ;;"
            )
    lines += ["    *) return 1 ;;", "  esac", "}"]
    lines += [
        "discodex_platform_asset() {",
        "  case \"$1:$2\" in",
    ]
    for platform in product["platforms"]:
        if platform["shellOs"] is None:
            continue
        asset = metadata["workerAssetTemplate"].replace("{target}", platform["target"]).replace("{extension}", platform["extension"])
        for arch in platform["shellArch"]:
            lines.append(
                f"    {platform['shellOs']}:{arch}) printf '%s' {shell_quote(asset)} ;;"
            )
    lines += ["    *) return 1 ;;", "  esac", "}"]
    lines += [
        "discodex_message() {",
        "  key=$1",
        "  locale=$2",
        "  shift 2",
        "  case \"$key:$locale\" in",
    ]
    for key, translations in sorted(messages.items()):
        for locale, template in sorted(translations.items()):
            printf_template = template.replace("{0}", "%s").replace("{1}", "%s")
            lines.append(
                f"    {key}:{locale}) printf {shell_quote(printf_template)} \"$@\" ;;"
            )
    lines += ["    *) return 2 ;;", "  esac", "}"]
    return "\n".join(lines) + "\n"


def generated_powershell(product, messages):
    metadata = product["product"]
    lines = [
        "# Generated from resources/product.json and resources/messages/*.json.",
        "# Regenerate with python scripts/product_resources.py generate.",
        "$script:DiscodexProduct = @{",
        f"    Name = {powershell_quote(metadata['name'])}",
        f"    DataDirectory = {powershell_quote(metadata['dataDirectory'])}",
        f"    ReleaseDownloadRoot = {powershell_quote(metadata['releaseDownloadRoot'].rstrip('/'))}",
        f"    ConfigFile = {powershell_quote(metadata['configFile'])}",
        f"    OwnershipFile = {powershell_quote(metadata['ownershipFile'])}",
        f"    InstallListFile = {powershell_quote(metadata['installationListFile'])}",
        f"    CredentialsFile = {powershell_quote(metadata['credentialsFile'])}",
        f"    StateDirectory = {powershell_quote(metadata['stateDirectory'])}",
        f"    BinaryDirectory = {powershell_quote(metadata['binaryDirectory'])}",
        f"    MessageAccessFile = {powershell_quote(metadata['messageAccessFile'])}",
        f"    DiscordCommandFile = {powershell_quote(metadata['discordCommandFile'])}",
        f"    RuntimeDirectory = {powershell_quote(metadata['runtimeDirectory'])}",
        f"    UpdatesFile = {powershell_quote(metadata['updatesFile'])}",
        "    SupportedLanguages = @(" + ", ".join(powershell_quote(value) for value in metadata["supportedLanguages"]) + ")",
        f"    InstallTempTemplate = {powershell_quote(metadata['installTempTemplate'])}",
        f"    UpdateTempTemplate = {powershell_quote(metadata['updateTempTemplate'])}",
        f"    InstallStagingTemplate = {powershell_quote(metadata['installStagingTemplate'])}",
        "    Platforms = @{",
    ]
    for platform in product["platforms"]:
        for arch in platform["powershellArch"]:
            asset = metadata["workerAssetTemplate"].replace("{target}", platform["target"]).replace("{extension}", platform["extension"])
            lines += [
                f"        {powershell_quote(arch)} = @{{",
                f"            Target = {powershell_quote(platform['target'])}",
                f"            Asset = {powershell_quote(asset)}",
                f"            Extension = {powershell_quote(platform['extension'])}",
                "        }",
            ]
    lines += ["    }", "}", "$script:DiscodexMessages = @{"]
    for key, translations in sorted(messages.items()):
        lines.append(f"    {powershell_quote(key)} = @{{")
        for locale, value in sorted(translations.items()):
            index = 0
            while "%s" in value:
                value = value.replace("%s", "{" + str(index) + "}", 1)
                index += 1
            lines.append(f"        {powershell_quote(locale)} = {powershell_quote(value)}")
        lines.append("    }")
    lines += [
        "}",
        "function Get-DiscodexMessage {",
        "    param([string]$Key, [string]$Language)",
        "    $template = $script:DiscodexMessages[$Key][$Language]",
        "    if ($null -eq $template) { throw \"Missing Discodex message: $Key ($Language)\" }",
        "    return $template",
        "}",
    ]
    return "\n".join(lines) + "\n"


def matrix(product, release):
    rows = []
    for platform in product["platforms"]:
        runner = platform["releaseRunner"] if release else platform["testRunner"]
        if runner is None:
            continue
        rows.append({
            "target": platform["target"],
            "runner": runner,
            "suffix": platform["extension"],
        })
    return json.dumps({"include": rows}, separators=(",", ":"))


def generate():
    product, messages = load_resources()
    (ROOT / "scripts/product.generated.sh").write_text(
        generated_shell(product, messages), encoding="utf-8"
    )
    (ROOT / "scripts/product.generated.ps1").write_text(
        generated_powershell(product, messages), encoding="utf-8"
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("generate", "release-matrix", "test-matrix"))
    args = parser.parse_args()
    product, messages = load_resources()
    if args.command == "generate":
        generate()
    else:
        print(matrix(product, args.command == "release-matrix"))


if __name__ == "__main__":
    main()
