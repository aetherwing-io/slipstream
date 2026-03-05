#!/usr/bin/env bash
set -euo pipefail

# ==========================================================================
#  Head-to-Head Benchmark: Traditional Read/Edit vs Slipstream MCP
#
#  Run from a REGULAR terminal (not inside Claude Code):
#    cd /Users/scottmeyer/projects/slipstream
#    bash docs/benchmark-h2h.sh [--model MODEL]
# ==========================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SETUP="$SCRIPT_DIR/benchmark-setup.py"
MCP_CONFIG="$PROJECT_DIR/.mcp.json"
REPORT="$SCRIPT_DIR/reports/h2h-results.md"

# Defaults
MODEL="${1:-sonnet}"
BUDGET="2.00"

# Parse args
while [[ $# -gt 0 ]]; do
    case $1 in
        --model) MODEL="$2"; shift 2;;
        --budget) BUDGET="$2"; shift 2;;
        *) shift;;
    esac
done

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

echo -e "${BOLD}================================================================${RESET}"
echo -e "${BOLD}  HEAD-TO-HEAD: Traditional Read/Edit vs Slipstream MCP${RESET}"
echo -e "${BOLD}  Model: ${MODEL} | Budget: \$${BUDGET}/contender${RESET}"
echo -e "${BOLD}================================================================${RESET}"
echo

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

DIR_A=$(mktemp -d /tmp/h2h-traditional-XXXX)
DIR_B=$(mktemp -d /tmp/h2h-slipstream-XXXX)
OUT_A="/tmp/h2h-result-traditional.json"
OUT_B="/tmp/h2h-result-slipstream.json"

cleanup() {
    rm -rf "$DIR_A" "$DIR_B" /tmp/h2h-prompt-a.txt /tmp/h2h-prompt-b.txt 2>/dev/null || true
}
trap cleanup EXIT

echo "Creating test files..."
python3 "$SETUP" create "$DIR_A"
python3 "$SETUP" create "$DIR_B"
echo

# Generate prompts to temp files (too large for shell variables on some systems)
PROMPT_FILE_A="/tmp/h2h-prompt-a.txt"
PROMPT_FILE_B="/tmp/h2h-prompt-b.txt"
python3 "$SETUP" prompt "$DIR_A" > "$PROMPT_FILE_A"
python3 "$SETUP" prompt "$DIR_B" > "$PROMPT_FILE_B"

# ---------------------------------------------------------------------------
# Contender A: Traditional (Read + Edit only)
# ---------------------------------------------------------------------------

echo -e "${CYAN}${BOLD}=== Contender A: Traditional (Read/Edit/Write/Glob/Grep) ===${RESET}"
echo "Starting..."

TIME_A_START=$(python3 -c "import time; print(time.time())")

env -i HOME="$HOME" PATH="$PATH" USER="$USER" TERM="$TERM" SHELL="$SHELL" claude -p \
    --output-format json \
    --model "$MODEL" \
    --no-session-persistence \
    --dangerously-skip-permissions \
    --max-budget-usd "$BUDGET" \
    --allowedTools "Read,Edit,Write,Glob,Grep" \
    --add-dir "$DIR_A" \
    "$(cat "$PROMPT_FILE_A")" \
    > "$OUT_A" 2>/tmp/h2h-stderr-a.txt || true

if [[ -s /tmp/h2h-stderr-a.txt ]]; then
    echo "  stderr (last 5 lines):"
    tail -5 /tmp/h2h-stderr-a.txt | sed 's/^/    /'
fi

TIME_A_END=$(python3 -c "import time; print(time.time())")
WALL_A=$(python3 -c "print(f'{$TIME_A_END - $TIME_A_START:.1f}')")

echo -e "Wall time: ${WALL_A}s"
echo "Verifying..."
python3 "$SETUP" verify "$DIR_A" || true
echo

# ---------------------------------------------------------------------------
# Contender B: Slipstream MCP
# ---------------------------------------------------------------------------

echo -e "${CYAN}${BOLD}=== Contender B: Slipstream MCP ===${RESET}"
echo "Starting..."

TIME_B_START=$(python3 -c "import time; print(time.time())")

env -i HOME="$HOME" PATH="$PATH" USER="$USER" TERM="$TERM" SHELL="$SHELL" claude -p \
    --output-format json \
    --model "$MODEL" \
    --no-session-persistence \
    --dangerously-skip-permissions \
    --max-budget-usd "$BUDGET" \
    --allowedTools "Read,Edit,Write,Glob,Grep,mcp__slipstream__slipstream,mcp__slipstream__slipstream_session,mcp__slipstream__slipstream_query,mcp__slipstream__slipstream_help" \
    --mcp-config "$MCP_CONFIG" \
    --add-dir "$DIR_B" \
    "$(cat "$PROMPT_FILE_B")" \
    > "$OUT_B" 2>/tmp/h2h-stderr-b.txt || true

if [[ -s /tmp/h2h-stderr-b.txt ]]; then
    echo "  stderr (last 5 lines):"
    tail -5 /tmp/h2h-stderr-b.txt | sed 's/^/    /'
fi

TIME_B_END=$(python3 -c "import time; print(time.time())")
WALL_B=$(python3 -c "print(f'{$TIME_B_END - $TIME_B_START:.1f}')")

echo -e "Wall time: ${WALL_B}s"
echo "Verifying..."
python3 "$SETUP" verify "$DIR_B" || true
echo

# ---------------------------------------------------------------------------
# Extract metrics from JSON output
# ---------------------------------------------------------------------------

echo -e "${BOLD}================================================================${RESET}"
echo -e "${BOLD}  RESULTS${RESET}"
echo -e "${BOLD}================================================================${RESET}"
echo

# Probe JSON structure and extract what we can
# The exact fields depend on Claude Code version — be defensive
extract_metrics() {
    local json_file="$1"
    local label="$2"

    if [[ ! -s "$json_file" ]]; then
        echo "  $label: No JSON output captured"
        return
    fi

    # Try to extract common fields — adapt based on actual schema
    echo "  $label:"

    # num_turns (tool call rounds)
    local turns=$(jq -r '.num_turns // .numTurns // "N/A"' "$json_file" 2>/dev/null)
    echo "    Turns: $turns"

    # Usage stats
    local input_tokens=$(jq -r '.usage.input_tokens // .inputTokens // .usage.inputTokens // "N/A"' "$json_file" 2>/dev/null)
    local output_tokens=$(jq -r '.usage.output_tokens // .outputTokens // .usage.outputTokens // "N/A"' "$json_file" 2>/dev/null)
    echo "    Input tokens: $input_tokens"
    echo "    Output tokens: $output_tokens"

    # Cost
    local cost=$(jq -r '.cost_usd // .costUsd // .usage.cost_usd // "N/A"' "$json_file" 2>/dev/null)
    echo "    Cost: \$$cost"

    # Session ID for reference
    local session=$(jq -r '.session_id // .sessionId // "N/A"' "$json_file" 2>/dev/null)
    echo "    Session: $session"
}

extract_metrics "$OUT_A" "Traditional"
echo
extract_metrics "$OUT_B" "Slipstream"
echo

echo "  Wall time:"
echo "    Traditional: ${WALL_A}s"
echo "    Slipstream:  ${WALL_B}s"
echo

# ---------------------------------------------------------------------------
# Dump JSON keys for debugging (first run — discover the schema)
# ---------------------------------------------------------------------------

echo -e "${BOLD}--- JSON Schema Discovery ---${RESET}"
echo "  Traditional top-level keys:"
jq -r 'keys[]' "$OUT_A" 2>/dev/null | sed 's/^/    /' || echo "    (no JSON)"
echo "  Slipstream top-level keys:"
jq -r 'keys[]' "$OUT_B" 2>/dev/null | sed 's/^/    /' || echo "    (no JSON)"
echo

# Also dump full JSON for post-analysis
echo "Raw JSON saved to:"
echo "  Traditional: $OUT_A"
echo "  Slipstream:  $OUT_B"
echo

# ---------------------------------------------------------------------------
# Save report
# ---------------------------------------------------------------------------

mkdir -p "$(dirname "$REPORT")"

VERIFY_A=$(python3 "$SETUP" verify "$DIR_A" 2>&1 | tail -1)
VERIFY_B=$(python3 "$SETUP" verify "$DIR_B" 2>&1 | tail -1)

cat > "$REPORT" << REPORT
# Head-to-Head Benchmark Results

**Date**: $(date +%Y-%m-%d)
**Model**: ${MODEL}
**Files**: 5 | **Edits**: 8

## Results

| Metric | Traditional | Slipstream |
|--------|-------------|------------|
| Wall time | ${WALL_A}s | ${WALL_B}s |
| Correctness | ${VERIFY_A} | ${VERIFY_B} |

## JSON Output

Raw JSON saved to:
- Traditional: \`${OUT_A}\`
- Slipstream: \`${OUT_B}\`

Inspect with:
\`\`\`bash
jq . ${OUT_A}  # Traditional
jq . ${OUT_B}  # Slipstream
\`\`\`

## Reproduction

\`\`\`bash
cd ${PROJECT_DIR}
bash docs/benchmark-h2h.sh --model ${MODEL}
\`\`\`
REPORT

echo -e "${GREEN}Report saved to: ${REPORT}${RESET}"
echo
echo -e "${BOLD}================================================================${RESET}"
echo -e "${BOLD}  Done. Inspect raw JSON for detailed tool call analysis.${RESET}"
echo -e "${BOLD}================================================================${RESET}"
