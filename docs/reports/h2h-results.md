# Head-to-Head Benchmark Results

**Date**: 2026-03-04
**Model**: sonnet
**Files**: 5 | **Edits**: 8

## Results

| Metric | Traditional | Slipstream |
|--------|-------------|------------|
| Wall time | 22.8s | 20.5s |
| Correctness | Result: 8/8 passed, 0/8 failed | Result: 8/8 passed, 0/8 failed |

## JSON Output

Raw JSON saved to:
- Traditional: `/tmp/h2h-result-traditional.json`
- Slipstream: `/tmp/h2h-result-slipstream.json`

Inspect with:
```bash
jq . /tmp/h2h-result-traditional.json  # Traditional
jq . /tmp/h2h-result-slipstream.json  # Slipstream
```

## Reproduction

```bash
cd /Users/scottmeyer/projects/slipstream
bash docs/benchmark-h2h.sh --model sonnet
```
