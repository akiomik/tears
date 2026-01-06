#!/usr/bin/env bash
# Compare two flamegraph files to measure optimization impact
#
# Usage: ./scripts/compare_flamegraphs.sh <before.svg> <after.svg>
#
# This script compares performance metrics between two flamegraph files
# to help measure the impact of optimizations.

set -euo pipefail

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

if [ $# -ne 2 ]; then
    echo "Usage: $0 <before.svg> <after.svg>"
    exit 1
fi

BEFORE_FILE="$1"
AFTER_FILE="$2"

if [ ! -f "$BEFORE_FILE" ]; then
    echo "Error: File '$BEFORE_FILE' not found"
    exit 1
fi

if [ ! -f "$AFTER_FILE" ]; then
    echo "Error: File '$AFTER_FILE' not found"
    exit 1
fi

echo -e "${BOLD}=== Flamegraph Comparison ===${NC}"
echo "Before: $BEFORE_FILE"
echo "After:  $AFTER_FILE"
echo ""

# Function to extract total samples from a flamegraph
get_total_samples() {
    local file="$1"
    grep -o '<title>[^<]*</title>' "$file" | \
        sed 's/<title>//g' | sed 's/<\/title>//g' | \
        grep -oE '\([0-9]+ samples,' | \
        sed 's/(\([0-9]*\) samples.*/\1/' | \
        awk '{sum+=$1} END {print sum}'
}

# Function to get samples for a pattern
get_samples_for_pattern() {
    local file="$1"
    local pattern="$2"
    grep -o '<title>[^<]*</title>' "$file" | \
        sed 's/<title>//g' | sed 's/<\/title>//g' | \
        grep -E "$pattern" | \
        grep -oE '\([0-9]+ samples,' | \
        sed 's/(\([0-9]*\) samples.*/\1/' | \
        awk '{sum+=$1} END {print sum+0}'
}

# Get total samples
BEFORE_TOTAL=$(get_total_samples "$BEFORE_FILE")
AFTER_TOTAL=$(get_total_samples "$AFTER_FILE")

echo -e "${BLUE}Total Samples:${NC}"
printf "  Before: %8d\n" "$BEFORE_TOTAL"
printf "  After:  %8d\n" "$AFTER_TOTAL"

if [ "$BEFORE_TOTAL" -gt 0 ]; then
    TOTAL_CHANGE=$(awk -v before="$BEFORE_TOTAL" -v after="$AFTER_TOTAL" \
        'BEGIN {printf "%.1f", ((after-before)/before)*100}')
    echo -e "  Change: ${BOLD}${TOTAL_CHANGE}%${NC}"
fi

echo ""

# Compare key metrics
echo -e "${BOLD}=== Key Metrics Comparison ===${NC}"
echo ""

compare_metric() {
    local name="$1"
    local pattern="$2"
    
    local before=$(get_samples_for_pattern "$BEFORE_FILE" "$pattern")
    local after=$(get_samples_for_pattern "$AFTER_FILE" "$pattern")
    
    local before_pct=$(awk -v samples="$before" -v total="$BEFORE_TOTAL" \
        'BEGIN {printf "%.1f", (samples/total)*100}')
    local after_pct=$(awk -v samples="$after" -v total="$AFTER_TOTAL" \
        'BEGIN {printf "%.1f", (samples/total)*100}')
    
    echo -e "${YELLOW}${name}:${NC}"
    printf "  Before: %6d samples (%5.1f%%)\n" "$before" "$before_pct"
    printf "  After:  %6d samples (%5.1f%%)\n" "$after" "$after_pct"
    
    if [ "$before" -gt 0 ]; then
        local change=$(awk -v before="$before" -v after="$after" \
            'BEGIN {printf "%.1f", ((after-before)/before)*100}')
        local abs_change=$((after - before))
        
        if (( $(echo "$change < -5.0" | bc -l) )); then
            echo -e "  ${GREEN}Improvement: ${abs_change} samples (${change}%)${NC}"
        elif (( $(echo "$change > 5.0" | bc -l) )); then
            echo -e "  ${RED}Regression: +${abs_change} samples (+${change}%)${NC}"
        else
            echo -e "  No significant change: ${change}%"
        fi
    fi
    echo ""
}

# Terminal size operations
compare_metric "Terminal Size Queries" "crossterm::terminal.*size|std::fs::OpenOptions.*open"

# Buffer diffing
compare_metric "Buffer Diffing" "Buffer::diff|Cell.*PartialEq"

# Rendering
compare_metric "Rendering Total" "ratatui.*terminal.*try_draw"

# Parking/idle
compare_metric "Idle/Parking" "park|psynch_cvwait"

# Tears framework
compare_metric "Tears Framework" "tears::"

echo -e "${BOLD}=== Summary ===${NC}"

if [ "$BEFORE_TOTAL" -gt 0 ] && [ "$AFTER_TOTAL" -gt 0 ]; then
    # Calculate improvement percentage
    IMPROVEMENT=$(awk -v before="$BEFORE_TOTAL" -v after="$AFTER_TOTAL" \
        'BEGIN {printf "%.1f", ((before-after)/before)*100}')
    
    if (( $(echo "$IMPROVEMENT > 5.0" | bc -l) )); then
        echo -e "${GREEN}✓ Overall performance improved by ${IMPROVEMENT}%${NC}"
    elif (( $(echo "$IMPROVEMENT < -5.0" | bc -l) )); then
        REGRESSION=$(awk -v imp="$IMPROVEMENT" 'BEGIN {printf "%.1f", -imp}')
        echo -e "${RED}✗ Performance regressed by ${REGRESSION}%${NC}"
    else
        echo -e "${YELLOW}≈ No significant performance change${NC}"
    fi
fi

echo ""
