#!/bin/bash

# Licensed to the Apache Software Foundation (ASF) under one
# or more contributor license agreements.  See the NOTICE file
# distributed with this work for additional information
# regarding copyright ownership.  The ASF licenses this file
# to you under the Apache License, Version 2.0 (the
# "License"); you may not use this file except in compliance
# with the License.  You may obtain a copy of the License at
#
#   http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing,
# software distributed under the License is distributed on an
# "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
# KIND, either express or implied.  See the License for the
# specific language governing permissions and limitations
# under the License.

# Function to get the git branch and commit hash
function get_git_info() {
    local git_info
    git_info=$(git log -1 --pretty=format:"%h")
    local git_branch
    git_branch=$(git rev-parse --abbrev-ref HEAD)
    echo "${git_branch}_${git_info}"
}

# OS detection
OS=$(uname -s)

# Function to check if process exists - OS specific implementations
function process_exists() {
    local check_pid=$1
    if [ "$OS" = "Darwin" ]; then
        ps -p "${check_pid}" >/dev/null
    else
        [[ -e /proc/${check_pid} ]]
    fi
}

# Function to send signal to process - OS specific implementations
function send_signal_to_pid() {
    local target_pid=$1
    local signal=$2
    if [ "$OS" = "Darwin" ]; then
        kill "-${signal}" "${target_pid}"
    else
        kill -s "${signal}" "${target_pid}"
    fi
}

# Function to wait for a process with specific name to exit
function wait_for_process() {
    local process_name=$1
    local timeout=$2
    local start_time
    start_time=$(date +%s)
    local end_time=$((start_time + timeout))
    local continue_outer_loop=false

    while [[ $(date +%s) -lt ${end_time} ]]; do
        continue_outer_loop=false
        local proc_pid
        for proc_pid in $(pgrep -x "${process_name}"); do
            if process_exists "${proc_pid}"; then
                sleep 0.1
                continue_outer_loop=true
                break
            fi
        done
        [[ $continue_outer_loop == true ]] && continue
        return 0
    done

    echo "Timeout waiting for process ${process_name} to exit."
    return 1
}

# Function to wait for a process with specific PID to exit
function wait_for_process_pid() {
    local wait_pid=$1
    local timeout=$2
    local start_time
    start_time=$(date +%s)
    local end_time=$((start_time + timeout))

    while [[ $(date +%s) -lt ${end_time} ]]; do
        if ! process_exists "${wait_pid}"; then
            return 0
        fi
        sleep 0.1
    done

    echo "Timeout waiting for process with PID ${wait_pid} to exit."
    return 1
}

# Function to send a signal to a process
function send_signal() {
    local process_name=$1
    local pids
    pids=$(pgrep -x "${process_name}") || true
    local signal=$2

    if [[ -n "${pids}" ]]; then
        local proc_pid
        for proc_pid in ${pids}; do
            if process_exists "${proc_pid}"; then
                send_signal_to_pid "${proc_pid}" "${signal}"
            fi
        done
    fi
}

# Function to exit with error if a process with the given PID is running
exit_if_process_is_not_running() {
    local check_pid="$1"

    if kill -0 "$check_pid" 2>/dev/null; then
        echo "Process with PID $check_pid is running."
        return 0
    else
        echo "Error: Process with PID $check_pid is not running."
        exit 1
    fi
}

# Exit hook for profile.sh
function on_exit_profile() {
    # Gracefully stop the server
    send_signal "iggy-server" "KILL"
    send_signal "iggy-bench" "KILL"
    send_signal "flamegraph" "KILL"
    send_signal "perf" "KILL"
}

# Exit hook for run-benches.sh
function on_exit_bench() {
    send_signal "iggy-server" "KILL"
    # Use exact match for iggy-bench to avoid killing iggy-bench-dashboard
    pids=$(pgrep -x "iggy-bench") || true
    if [[ -n "${pids}" ]]; then
        local bench_pid
        for bench_pid in ${pids}; do
            if process_exists "${bench_pid}"; then
                send_signal_to_pid "${bench_pid}" "KILL"
            fi
        done
    fi
}

# ---------------------------------------------------------------------------
# Example Testing Utilities
# Used by scripts/run-examples.sh to manage iggy-server lifecycle and
# execute SDK examples parsed from README files.
# ---------------------------------------------------------------------------

# Find and validate the iggy-server binary.
# Args: [target_arch]
# Prints the binary path to stdout. Returns 1 if not found.
function find_server_binary() {
    local target="${1:-}"
    local server_bin
    if [ -n "${target}" ]; then
        server_bin="target/${target}/debug/iggy-server"
    else
        server_bin="target/debug/iggy-server"
    fi
    if [ ! -f "${server_bin}" ]; then
        echo "Error: Server binary not found at ${server_bin}" >&2
        if [ -n "${target}" ]; then
            echo "  Build with: cargo build --target ${target} --bin iggy-server" >&2
        else
            echo "  Build with: cargo build --bin iggy-server" >&2
        fi
        return 1
    fi
    echo "${server_bin}"
}

# Find and validate the iggy CLI binary.
# Args: [target_arch]
# Prints the binary path to stdout. Returns 1 if not found.
function find_cli_binary() {
    local target="${1:-}"
    local cli_bin
    if [ -n "${target}" ]; then
        cli_bin="target/${target}/debug/iggy"
    else
        cli_bin="target/debug/iggy"
    fi
    if [ ! -f "${cli_bin}" ]; then
        echo "Error: CLI binary not found at ${cli_bin}" >&2
        if [ -n "${target}" ]; then
            echo "  Build with: cargo build --target ${target} --bin iggy --examples" >&2
        else
            echo "  Build with: cargo build --bin iggy --examples" >&2
        fi
        return 1
    fi
    echo "${cli_bin}"
}

# Remove local_data directory and server log/pid files.
# Args: log_file pid_file
function clean_server_data() {
    local log_file="$1"
    local pid_file="$2"
    test -d local_data && rm -fr local_data || true
    rm -f "${log_file}" "${pid_file}"
}

# Start iggy-server in the background.
# Args: server_bin log_file pid_file [extra_server_args...]
function start_server() {
    local server_bin="$1"
    local log_file="$2"
    local pid_file="$3"
    shift 3
    echo "Starting server from ${server_bin}..."
    IGGY_ROOT_USERNAME=iggy IGGY_ROOT_PASSWORD=iggy "${server_bin}" "$@" &>"${log_file}" &
    echo $! >"${pid_file}"
}

# Start iggy-server with TCP TLS enabled in the background.
# Args: server_bin log_file pid_file [extra_server_args...]
function start_tls_server() {
    local server_bin="$1"
    local log_file="$2"
    local pid_file="$3"
    shift 3
    echo "Starting TLS-enabled server from ${server_bin}..."
    IGGY_TCP_TLS_ENABLED=true \
    IGGY_TCP_TLS_SELF_SIGNED=true \
    IGGY_ROOT_USERNAME=iggy \
    IGGY_ROOT_PASSWORD=iggy \
    "${server_bin}" "$@" &>"${log_file}" &
    echo $! >"${pid_file}"
}

# Block until "has started" appears in the server log.
# Args: log_file [timeout_seconds (default 300)]
function wait_for_server() {
    local log_file="$1"
    local timeout="${2:-300}"
    local elapsed=0
    while ! grep -q "has started" "${log_file}" 2>/dev/null; do
        if [ ${elapsed} -gt "${timeout}" ]; then
            echo "Server did not start within ${timeout} seconds."
            ps fx 2>/dev/null || true
            cat "${log_file}" 2>/dev/null || true
            return 1
        fi
        echo "Waiting for Iggy server to start... ${elapsed}"
        sleep 1
        ((elapsed += 1))
    done
}

# Stop the server whose PID is stored in pid_file.
# Args: pid_file
function stop_server() {
    local pid_file="$1"
    if [ -f "${pid_file}" ]; then
        kill -TERM "$(cat "${pid_file}")" 2>/dev/null || true
        rm -f "${pid_file}"
    fi
}

# Print pass/fail, dump the log on failure, then remove temp files.
# Args: exit_code log_file pid_file
function report_test_results() {
    local exit_code="$1"
    local log_file="$2"
    local pid_file="$3"
    if [ "${exit_code}" -eq 0 ]; then
        echo "Test passed"
    else
        echo "Test failed, see log file:"
        cat "${log_file}" 2>/dev/null || true
    fi
    rm -f "${log_file}" "${pid_file}"
}
