#!/bin/sh
set -eu

MODE=${1:-}
LABEL=${2:-}
PRELOAD_LIBRARY=${3:-}

WORK_ROOT=${WORK_ROOT:-/tmp/rsmalloc-mariadb-bench}
TEMPLATE_DIR=${TEMPLATE_DIR:-$WORK_ROOT/template}
RUN_ROOT=${RUN_ROOT:-$WORK_ROOT/runs}
RESULT_ROOT=${RESULT_ROOT:-$WORK_ROOT/results}
SOCKET=${SOCKET:-$WORK_ROOT/mariadb.sock}
PID_FILE=${PID_FILE:-$WORK_ROOT/mariadb.pid}
PHASE_FILE=${PHASE_FILE:-$WORK_ROOT/phase}

TABLES=${TABLES:-8}
TABLE_SIZE=${TABLE_SIZE:-250000}
THREADS=${THREADS:-32}
WARMUP_SECONDS=${WARMUP_SECONDS:-30}
RUN_SECONDS=${RUN_SECONDS:-60}
IDLE_SECONDS=${IDLE_SECONDS:-15}
SAMPLE_INTERVAL=${SAMPLE_INTERVAL:-0.20}
ADMIN_TIMEOUT=${ADMIN_TIMEOUT:-10}
SHUTDOWN_TIMEOUT=${SHUTDOWN_TIMEOUT:-30}
BUFFER_POOL_SIZE=${BUFFER_POOL_SIZE:-256M}
MAX_CONNECTIONS=${MAX_CONNECTIONS:-256}

MARIADBD=${MARIADBD:-mariadbd}
INSTALL_DB=${INSTALL_DB:-mariadb-install-db}
MARIADB=${MARIADB:-mariadb}
MARIADB_ADMIN=${MARIADB_ADMIN:-mariadb-admin}
SYSBENCH=${SYSBENCH:-sysbench}

SERVER_PID=
SAMPLER_PID=
CURRENT_DATA_DIR=
CURRENT_RESULT_DIR=

usage() {
    cat <<'EOF'
Usage:
  ./run.sh prepare
  ./run.sh run LABEL [PRELOAD_LIBRARY]

Examples:
  ./run.sh prepare
  ./run.sh run glibc
  ./run.sh run rsmalloc ../../target/release/librsmalloc.so
  ./run.sh run mimalloc /usr/lib/libmimalloc.so

Configuration is provided through environment variables. See README.md.
EOF
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

check_dependencies() {
    require_command "$MARIADBD"
    require_command "$INSTALL_DB"
    require_command "$MARIADB"
    require_command "$MARIADB_ADMIN"
    require_command "$SYSBENCH"
    require_command awk
    require_command cp
    require_command timeout
}

server_args() {
    cat <<EOF
--no-defaults
--datadir=$CURRENT_DATA_DIR
--socket=$SOCKET
--pid-file=$PID_FILE
--skip-networking
--user=$(id -un)
--tmpdir=$CURRENT_DATA_DIR/tmp
--log-error=$CURRENT_RESULT_DIR/mariadb.log
--innodb-buffer-pool-size=$BUFFER_POOL_SIZE
--innodb-log-file-size=64M
--innodb-flush-log-at-trx-commit=2
--innodb-doublewrite=0
--performance-schema=OFF
--max-connections=$MAX_CONNECTIONS
--table-open-cache=256
--thread-cache-size=0
EOF
}

start_server() {
    rm -f "$SOCKET" "$PID_FILE"
    mkdir -p "$CURRENT_DATA_DIR/tmp" "$CURRENT_RESULT_DIR"

    # Word splitting is intentional: server_args emits one option per line.
    # shellcheck disable=SC2046
    if [ -n "$PRELOAD_LIBRARY" ]; then
        if [ ! -r "$PRELOAD_LIBRARY" ]; then
            echo "preload library is not readable: $PRELOAD_LIBRARY" >&2
            exit 1
        fi
        env LD_PRELOAD="$PRELOAD_LIBRARY" "$MARIADBD" $(server_args) &
    else
        "$MARIADBD" $(server_args) &
    fi
    SERVER_PID=$!

    attempts=0
    while ! "$MARIADB_ADMIN" --no-defaults --socket="$SOCKET" --user=root ping >/dev/null 2>&1; do
        if ! kill -0 "$SERVER_PID" >/dev/null 2>&1; then
            echo "MariaDB exited during startup; see $CURRENT_RESULT_DIR/mariadb.log" >&2
            wait "$SERVER_PID" || true
            exit 1
        fi
        attempts=$((attempts + 1))
        if [ "$attempts" -ge 300 ]; then
            echo "MariaDB did not become ready within 30 seconds" >&2
            exit 1
        fi
        sleep 0.1
    done

    cat "/proc/$SERVER_PID/maps" > "$CURRENT_RESULT_DIR/maps.txt"
    if [ -n "$PRELOAD_LIBRARY" ] && ! grep -F "$(basename "$PRELOAD_LIBRARY")" "$CURRENT_RESULT_DIR/maps.txt" >/dev/null; then
        echo "warning: preload library is not visible in /proc/$SERVER_PID/maps" >&2
    fi
}

stop_server() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
        if ! timeout "$ADMIN_TIMEOUT" "$MARIADB_ADMIN" --no-defaults --connect-timeout=5 --socket="$SOCKET" --user=root shutdown >/dev/null 2>&1; then
            echo "mariadb-admin shutdown did not complete within ${ADMIN_TIMEOUT}s" >&2
        fi

        waited=0
        while kill -0 "$SERVER_PID" >/dev/null 2>&1 && [ "$waited" -lt "$SHUTDOWN_TIMEOUT" ]; do
            sleep 1
            waited=$((waited + 1))
        done

        if kill -0 "$SERVER_PID" >/dev/null 2>&1; then
            echo "MariaDB did not exit after ${SHUTDOWN_TIMEOUT}s; sending TERM" >&2
            kill "$SERVER_PID" >/dev/null 2>&1 || true
            sleep 2
        fi
        if kill -0 "$SERVER_PID" >/dev/null 2>&1; then
            echo "MariaDB still did not exit; sending KILL" >&2
            kill -9 "$SERVER_PID" >/dev/null 2>&1 || true
        fi
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    SERVER_PID=
}

stop_sampler() {
    if [ -n "$SAMPLER_PID" ] && kill -0 "$SAMPLER_PID" >/dev/null 2>&1; then
        kill "$SAMPLER_PID" >/dev/null 2>&1 || true
        wait "$SAMPLER_PID" 2>/dev/null || true
    fi
    SAMPLER_PID=
}

cleanup() {
    stop_sampler
    stop_server
}
trap cleanup EXIT INT TERM

sysbench_common() {
    workload=$1
    shift
    "$SYSBENCH" "$workload" \
        --mysql-socket="$SOCKET" \
        --mysql-user=root \
        --mysql-db=allocator_bench \
        --tables="$TABLES" \
        --table-size="$TABLE_SIZE" \
        "$@"
}

sample_memory() {
    output=$1
    echo "elapsed_ms,phase,rss_kb,hwm_kb,vmsize_kb,pss_kb,anonymous_kb,private_kb,swap_kb" > "$output"
    start=$(date +%s%N)

    while kill -0 "$SERVER_PID" >/dev/null 2>&1; do
        now=$(date +%s%N)
        elapsed_ms=$(((now - start) / 1000000))
        phase=$(cat "$PHASE_FILE" 2>/dev/null || echo unknown)

        status_values=$(awk '
            /^VmRSS:/ { rss=$2 }
            /^VmHWM:/ { hwm=$2 }
            /^VmSize:/ { size=$2 }
            END { printf "%d,%d,%d", rss+0, hwm+0, size+0 }
        ' "/proc/$SERVER_PID/status")

        memory_values=$(awk '
            /^Pss:/ { pss += $2 }
            /^Anonymous:/ { anonymous += $2 }
            /^Private_Clean:/ { private += $2 }
            /^Private_Dirty:/ { private += $2 }
            /^Swap:/ { swapped += $2 }
            END { printf "%d,%d,%d,%d", pss+0, anonymous+0, private+0, swapped+0 }
        ' "/proc/$SERVER_PID/smaps_rollup")

        echo "$elapsed_ms,$phase,$status_values,$memory_values" >> "$output"
        sleep "$SAMPLE_INTERVAL"
    done
}

print_latest_memory() {
    samples=$1
    phase=$2
    awk -F, -v wanted="$phase" '
        NR > 1 && $2 == wanted {
            rss=$3; pss=$6; anonymous=$7; private=$8; swap=$9
        }
        END {
            if (rss != "")
                printf "[%s] latest: RSS %.1f MiB, PSS %.1f MiB, anonymous %.1f MiB, private %.1f MiB, swap %.1f MiB\n", wanted, rss/1024, pss/1024, anonymous/1024, private/1024, swap/1024
            else
                printf "[%s] no memory sample captured\n", wanted
        }
    ' "$samples"
}

write_summary() {
    samples=$1
    summary=$2
    awk -F, '
        NR == 1 { next }
        {
            phase=$2
            if ($3 > rss[phase]) rss[phase]=$3
            if ($4 > hwm[phase]) hwm[phase]=$4
            if ($5 > vmsize[phase]) vmsize[phase]=$5
            if ($6 > pss[phase]) pss[phase]=$6
            if ($7 > anonymous[phase]) anonymous[phase]=$7
            if ($8 > private[phase]) private[phase]=$8
            if ($9 > swap[phase]) swap[phase]=$9
        }
        END {
            print "phase,peak_rss_kb,peak_hwm_kb,peak_vmsize_kb,peak_pss_kb,peak_anonymous_kb,peak_private_kb,peak_swap_kb"
            order[1]="startup"; order[2]="warmup"; order[3]="workload"; order[4]="idle"
            for (i=1; i<=4; i++) {
                p=order[i]
                if (p in rss)
                    print p "," rss[p] "," hwm[p] "," vmsize[p] "," pss[p] "," anonymous[p] "," private[p] "," swap[p]
            }
        }
    ' "$samples" > "$summary"
}

prepare() {
    check_dependencies
    rm -rf "$TEMPLATE_DIR"
    mkdir -p "$TEMPLATE_DIR" "$RUN_ROOT" "$RESULT_ROOT"

    "$INSTALL_DB" \
        --no-defaults \
        --datadir="$TEMPLATE_DIR" \
        --auth-root-authentication-method=normal \
        --skip-test-db >/dev/null

    CURRENT_DATA_DIR=$TEMPLATE_DIR
    CURRENT_RESULT_DIR=$WORK_ROOT/prepare
    PRELOAD_LIBRARY=
    start_server

    "$MARIADB" --no-defaults --socket="$SOCKET" --user=root \
        -e "CREATE DATABASE allocator_bench;"

    sysbench_common oltp_read_write prepare > "$CURRENT_RESULT_DIR/sysbench-prepare.txt"
    stop_server

    echo "prepared template: $TEMPLATE_DIR"
}

run_variant() {
    check_dependencies
    if [ -z "$LABEL" ]; then
        usage
        exit 1
    fi
    if [ ! -d "$TEMPLATE_DIR/mysql" ]; then
        echo "template is missing; run './run.sh prepare' first" >&2
        exit 1
    fi

    CURRENT_DATA_DIR=$RUN_ROOT/$LABEL
    CURRENT_RESULT_DIR=$RESULT_ROOT/$LABEL
    rm -rf "$CURRENT_DATA_DIR" "$CURRENT_RESULT_DIR"
    mkdir -p "$CURRENT_DATA_DIR" "$CURRENT_RESULT_DIR"
    cp -a --reflink=auto "$TEMPLATE_DIR/." "$CURRENT_DATA_DIR/"

    {
        echo "label=$LABEL"
        echo "preload_library=${PRELOAD_LIBRARY:-system-default}"
        echo "tables=$TABLES"
        echo "table_size=$TABLE_SIZE"
        echo "threads=$THREADS"
        echo "warmup_seconds=$WARMUP_SECONDS"
        echo "run_seconds=$RUN_SECONDS"
        echo "idle_seconds=$IDLE_SECONDS"
        echo "sample_interval=$SAMPLE_INTERVAL"
        echo "buffer_pool_size=$BUFFER_POOL_SIZE"
        uname -a
        "$MARIADBD" --version
        "$SYSBENCH" --version
    } > "$CURRENT_RESULT_DIR/environment.txt" 2>&1

    echo "[$LABEL] starting MariaDB"
    echo startup > "$PHASE_FILE"
    start_server
    echo "[$LABEL] server PID: $SERVER_PID"
    sample_memory "$CURRENT_RESULT_DIR/memory.csv" &
    SAMPLER_PID=$!
    sleep "$SAMPLE_INTERVAL"
    print_latest_memory "$CURRENT_RESULT_DIR/memory.csv" startup

    echo "[$LABEL] warmup: ${WARMUP_SECONDS}s with $THREADS threads"
    echo warmup > "$PHASE_FILE"
    sysbench_common oltp_read_only run \
        --threads="$THREADS" \
        --time="$WARMUP_SECONDS" \
        --report-interval=0 > "$CURRENT_RESULT_DIR/warmup.txt"
    print_latest_memory "$CURRENT_RESULT_DIR/memory.csv" warmup

    echo "[$LABEL] read/write workload: ${RUN_SECONDS}s with $THREADS threads"
    echo workload > "$PHASE_FILE"
    sysbench_common oltp_read_write run \
        --threads="$THREADS" \
        --time="$RUN_SECONDS" \
        --report-interval=1 > "$CURRENT_RESULT_DIR/workload.txt"
    print_latest_memory "$CURRENT_RESULT_DIR/memory.csv" workload

    echo "[$LABEL] idle: ${IDLE_SECONDS}s"
    echo idle > "$PHASE_FILE"
    sleep "$IDLE_SECONDS"
    print_latest_memory "$CURRENT_RESULT_DIR/memory.csv" idle

    stop_sampler
    write_summary "$CURRENT_RESULT_DIR/memory.csv" "$CURRENT_RESULT_DIR/memory-summary.csv"
    echo "[$LABEL] stopping MariaDB"
    stop_server

    echo "results: $CURRENT_RESULT_DIR"
    cat "$CURRENT_RESULT_DIR/memory-summary.csv"
}

case "$MODE" in
    prepare)
        prepare
        ;;
    run)
        run_variant
        ;;
    *)
        usage
        exit 1
        ;;
esac
