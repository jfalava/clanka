#!/bin/sh
# Makes a Hermes profile that is one companion and nothing else. clanka's two
# tools are the only toolset on every platform and are listed directly rather
# than behind Hermes's tool-search bridge; Hermes's own memory, user profile,
# coding brief, workspace snapshot, toolchain probe and agent-style guidance
# are off; the terminal and Telegram notes are a line each; and SOUL.md is the
# persona. Each turn then carries the persona and the conversation instead of
# Hermes's whole toolbox: on Haiku the system prompt and tools went from about
# 19k tokens on a stock profile to about 1.2k, and the cold first turn from
# about $0.003 to about $0.0004.
#
#   hermes/profile.sh <profile> <soul.md>      after hermes/install.sh
#
# Safe to run again: it sets the same values. Run it after editing the persona
# to refresh SOUL.md. A gateway already serving the profile re-reads its config
# on its own and reloads the plugin when it is enabled here.
set -eu
profile=$1
soul=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
root=$(cd "$(dirname "$0")/.." && pwd)
home=${HERMES_HOME:-$HOME/.hermes}
clanka=${CLANKA:-$root/target/release/clanka}
[ -x "$clanka" ] || clanka=clanka

[ -d "$home/profiles/$profile" ] || hermes profile create "$profile"
h() { hermes -p "$profile" "$@"; }
h plugins enable clanka
h config set plugins.entries.clanka.settings.soul "$soul"
for platform in cli telegram discord slack whatsapp whatsapp_cloud signal bluebubbles email \
    mattermost matrix dingtalk feishu wecom wecom_callback weixin qqbot yuanbao webhook api_server; do
    h config set "platform_toolsets.$platform" "[clanka]"
done
h config set tools.tool_search.enabled off
h config set memory.memory_enabled false
h config set memory.user_profile_enabled false
h config set agent.coding_context off
h config set agent.environment_probe false
h config set agent.task_completion_guidance false
h config set agent.parallel_tool_call_guidance false
h config set agent.bot_mode_protocol false
h config set platform_hints.cli.replace \
    "Plain terminal: markdown does not render, so write plain text."
h config set platform_hints.telegram.replace \
    "You are on Telegram: short messages, *bold* and _italic_ render, nothing fancier."
"$clanka" soul persona "$soul" > "$home/profiles/$profile/SOUL.md"
echo "Profile $profile is $(grep -m1 '^name' "$soul" | cut -d'"' -f2): hermes -p $profile chat"
