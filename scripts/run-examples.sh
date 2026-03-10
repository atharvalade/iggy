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

set -euo pipefail

# Unified script to run SDK examples from README.md files.
#
# Usage: ./scripts/run-examples.sh --language <rust|go|python|node|java|csharp|all> [OPTIONS]
#
# Options:
#   --language     Language to test (required): rust, go, python, node, java, csharp, or all
#   --target       Target architecture for Rust server binary (e.g., x86_64-unknown-linux-musl)
#   --goos         Target OS for Go examples (e.g., linux, darwin)
#   --goarch       Target architecture for Go examples (e.g., amd64, arm64)
#   --csharpos     Target OS for C# examples
#   --csharparch   Target architecture for C# examples
#   --skip-tls     Skip TLS server pass
#   --tls-only     Only run TLS server pass
#
# For each selected language this script:
#   1. Starts a plain iggy-server, runs examples parsed from README files, stops the server.
#   2. Starts a TLS-enabled iggy-server, runs TLS-capable examples, stops the server.
#
# All example commands are extracted from their respective README.md files (single source of truth).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

source "${SCRIPT_DIR}/utils.sh"

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
readonly LOG_FILE="iggy-server.log"
readonly PID_FILE="iggy-server.pid"
readonly SERVER_TIMEOUT=300

# ---------------------------------------------------------------------------
# Options (set via command-line arguments)
# ---------------------------------------------------------------------------
LANGUAGE=""
TARGET=""
GOOS=""
GOARCH=""
CSOS=""
CSARCH=""
SKIP_TLS=false
TLS_ONLY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --language)   LANGUAGE="$2";  shift 2 ;;
        --target)     TARGET="$2";    shift 2 ;;
        --goos)       GOOS="$2";      shift 2 ;;
        --goarch)     GOARCH="$2";    shift 2 ;;
        --csharpos)   CSOS="$2";      shift 2 ;;
        --csharparch) CSARCH="$2";    shift 2 ;;
        --skip-tls)   SKIP_TLS=true;  shift ;;
        --tls-only)   TLS_ONLY=true;  shift ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 --language <rust|go|python|node|java|csharp|all> [OPTIONS]"
            exit 1
            ;;
    esac
done

if [ -z "${LANGUAGE}" ]; then
    echo "Error: --language is required"
    echo "Usage: $0 --language <rust|go|python|node|java|csharp|all> [OPTIONS]"
    exit 1
fi

readonly SUPPORTED_LANGUAGES="rust node go python java csharp"
if [ "${LANGUAGE}" != "all" ]; then
    if ! echo "${SUPPORTED_LANGUAGES}" | grep -qw "${LANGUAGE}"; then
        echo "Error: unsupported language '${LANGUAGE}'"
        echo "Supported: ${SUPPORTED_LANGUAGES}, all"
        exit 1
    fi
fi

cd "${ROOT_DIR}"

# Ensure server is stopped and working directory is restored on exit.
trap 'stop_server "${ROOT_DIR}/${PID_FILE}"; cd "${ROOT_DIR}"' EXIT

# ===========================================================================
# Rust examples
# ===========================================================================
run_rust_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    find_cli_binary "${TARGET}" > /dev/null || return 1

    if [ "${tls_mode}" = "tls" ]; then
        # TLS pass: run only commands that already contain TLS flags in the README.
        # Not all Rust examples support TLS args (only those using shared/args.rs do),
        # so we cannot blindly append --tcp-tls-enabled to every command.
        # When TLS example commands are added to the READMEs, they will be matched by
        # the pattern below and executed against the TLS-enabled server.
        local found_tls_commands=false
        for readme_file in "${ROOT_DIR}/README.md" "${ROOT_DIR}/examples/rust/README.md"; do
            [ -f "${readme_file}" ] || continue
            while IFS= read -r command; do
                command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
                [ -z "${command}" ] && continue
                found_tls_commands=true

                if [ -n "${TARGET}" ]; then
                    command="${command//cargo run /cargo run --target ${TARGET} }"
                fi

                echo -e "\e[33mChecking TLS example from ${readme_file}:\e[0m ${command}"
                echo ""

                set +e
                eval "${command}"
                exit_code=$?
                set -e

                if [ "${exit_code}" -ne 0 ]; then
                    echo -e "\e[31mTLS example failed:\e[0m ${command}"
                    return "${exit_code}"
                fi
                sleep 2
            done < <(grep -E "^cargo run --example.*--tcp-tls-enabled" "${readme_file}")
        done
        if [ "${found_tls_commands}" = false ]; then
            echo "No TLS examples found in Rust READMEs yet, skipping."
        fi
        return 0
    fi

    # Plain pass: CLI commands from root README (lines starting with backtick + cargo r)
    while IFS= read -r command; do
        command=$(echo "${command}" | tr -d '`')
        if [ -n "${TARGET}" ]; then
            command=$(echo "${command}" | sed "s|cargo r |cargo r --target ${TARGET} |g" \
                                        | sed "s|cargo run |cargo run --target ${TARGET} |g")
        fi
        echo -e "\e[33mChecking CLI command:\e[0m ${command}"
        echo ""

        set +e
        eval "${command}"
        exit_code=$?
        set -e

        if [ "${exit_code}" -ne 0 ]; then
            echo -e "\e[31mCLI command failed:\e[0m ${command}"
            return "${exit_code}"
        fi
    done < <(grep -E '^\`cargo r --bin iggy -- ' "${ROOT_DIR}/README.md")

    # Plain pass: example commands from root + rust README
    for readme_file in "${ROOT_DIR}/README.md" "${ROOT_DIR}/examples/rust/README.md"; do
        [ -f "${readme_file}" ] || continue

        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            if [ -n "${TARGET}" ]; then
                command="${command//cargo run /cargo run --target ${TARGET} }"
            fi

            echo -e "\e[33mChecking example from ${readme_file}:\e[0m ${command}"
            echo ""

            set +e
            eval "${command}"
            exit_code=$?
            set -e

            if [ "${exit_code}" -ne 0 ]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E "^cargo run --example" "${readme_file}")
    done

    return 0
}

# ===========================================================================
# Node.js examples
# ===========================================================================
run_node_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    if [ "${tls_mode}" = "tls" ]; then
        echo "No TLS examples available for Node yet, skipping."
        return 0
    fi

    export DEBUG=iggy:examples

    # CLI commands from root README
    while IFS= read -r command; do
        command=$(echo "${command}" | tr -d '`')
        echo -e "\e[33mChecking CLI command:\e[0m ${command}"
        echo ""

        set +e
        eval "${command}"
        exit_code=$?
        set -e

        if [ "${exit_code}" -ne 0 ]; then
            echo -e "\e[31mCLI command failed:\e[0m ${command}"
            return "${exit_code}"
        fi
    done < <(grep -E '^\`cargo r --bin iggy -- ' "${ROOT_DIR}/README.md")

    cd "${ROOT_DIR}/examples/node"

    # Node example commands from examples/node/README.md
    local readme_file="${ROOT_DIR}/examples/node/README.md"
    if [ -f "${readme_file}" ]; then
        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            echo -e "\e[33mChecking example from examples/node/README.md:\e[0m ${command}"
            echo ""

            set +e
            eval "${command}"
            exit_code=$?
            set -e

            if [ "${exit_code}" -ne 0 ]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                cd "${ROOT_DIR}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E "^(npm run|tsx)" "${readme_file}")
    fi

    cd "${ROOT_DIR}"
    return 0
}

# ===========================================================================
# Go examples
# ===========================================================================
run_go_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    if [ "${tls_mode}" = "tls" ]; then
        echo "No TLS examples available for Go yet, skipping."
        return 0
    fi

    cd "${ROOT_DIR}/examples/go"

    local readme_file="README.md"
    if [ -f "${readme_file}" ]; then
        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            [ -n "${GOOS}" ] && command="GOOS=${GOOS} ${command}"
            [ -n "${GOARCH}" ] && command="GOARCH=${GOARCH} ${command}"

            echo -e "\e[33mChecking example from examples/go/README.md:\e[0m ${command}"
            echo ""

            set +e
            eval "${command}"
            exit_code=$?
            set -e

            if [ "${exit_code}" -ne 0 ]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                cd "${ROOT_DIR}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E "^go run" "${readme_file}")
    fi

    cd "${ROOT_DIR}"
    return 0
}

# ===========================================================================
# Python examples
# ===========================================================================
run_python_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    if [ "${tls_mode}" = "tls" ]; then
        echo "No TLS examples available for Python yet, skipping."
        return 0
    fi

    cd "${ROOT_DIR}/examples/python"

    echo "Syncing Python dependencies with uv..."
    uv sync --frozen

    local readme_file="README.md"
    if [ -f "${readme_file}" ]; then
        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            echo -e "\e[33mChecking example from examples/python/README.md:\e[0m ${command}"
            echo ""

            set +e
            eval "timeout 10 ${command}"
            local test_exit_code=$?
            set -e

            # timeout exit code 124 is expected for long-running examples
            if [[ $test_exit_code -ne 0 && $test_exit_code -ne 124 ]]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                exit_code=$test_exit_code
                rm -rf .venv
                cd "${ROOT_DIR}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E "^uv run " "${readme_file}")
    fi

    rm -rf .venv
    cd "${ROOT_DIR}"
    return 0
}

# ===========================================================================
# Java examples
# ===========================================================================
run_java_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    if [ "${tls_mode}" = "tls" ]; then
        echo "No TLS examples available for Java yet, skipping."
        return 0
    fi

    cd "${ROOT_DIR}/examples/java"

    local readme_file="README.md"
    if [ -f "${readme_file}" ]; then
        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            echo -e "\e[33mChecking example from examples/java/README.md:\e[0m ${command}"
            echo ""

            set +e
            eval "${command}"
            exit_code=$?
            set -e

            if [ "${exit_code}" -ne 0 ]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                cd "${ROOT_DIR}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E '^\./gradlew' "${readme_file}")
    fi

    cd "${ROOT_DIR}"
    return 0
}

# ===========================================================================
# C# examples
# ===========================================================================
run_csharp_examples() {
    local tls_mode="${1:-plain}"
    local exit_code=0

    if [ "${tls_mode}" = "tls" ]; then
        echo "No TLS examples available for C# yet, skipping."
        return 0
    fi

    # CLI commands from root README
    while IFS= read -r command; do
        command=$(echo "${command}" | tr -d '`')

        if [ -n "${CSOS}" ]; then
            command="${command//dotnet run /dotnet run --os ${CSOS} }"
        fi
        if [ -n "${CSARCH}" ]; then
            command="${command//dotnet run /dotnet run --arch ${CSARCH} }"
        fi

        echo -e "\e[33mChecking CLI command:\e[0m ${command}"
        echo ""

        set +e
        eval "${command}"
        exit_code=$?
        set -e

        if [ "${exit_code}" -ne 0 ]; then
            echo -e "\e[31mCLI command failed:\e[0m ${command}"
            return "${exit_code}"
        fi
    done < <(grep -E '^\`cargo r --bin iggy -- ' "${ROOT_DIR}/README.md")

    # C# example commands from root + csharp README
    for readme_file in "${ROOT_DIR}/README.md" "${ROOT_DIR}/examples/csharp/README.md"; do
        [ -f "${readme_file}" ] || continue

        while IFS= read -r command; do
            command=$(echo "${command}" | tr -d '`' | sed 's/^#.*//')
            [ -z "${command}" ] && continue

            if [ -n "${CSOS}" ]; then
                command="${command//dotnet run /dotnet run --os ${CSOS} }"
            fi
            if [ -n "${CSARCH}" ]; then
                command="${command//dotnet run /dotnet run --arch ${CSARCH} }"
            fi

            echo -e "\e[33mChecking example from ${readme_file}:\e[0m ${command}"
            echo ""

            set +e
            eval "${command}"
            exit_code=$?
            set -e

            if [ "${exit_code}" -ne 0 ]; then
                echo -e "\e[31mExample command failed:\e[0m ${command}"
                return "${exit_code}"
            fi
            sleep 2
        done < <(grep -E "^dotnet run --project" "${readme_file}")
    done

    return 0
}

# ===========================================================================
# Orchestration: run examples for a single language in a given mode
# ===========================================================================
run_language() {
    local lang="$1"
    local mode="$2"
    local exit_code=0

    echo ""
    echo "========================================"
    echo "  ${lang} examples (${mode})"
    echo "========================================"
    echo ""

    clean_server_data "${LOG_FILE}" "${PID_FILE}"

    local server_bin
    server_bin=$(find_server_binary "${TARGET}") || return 1
    echo "Using server binary at ${server_bin}"

    if [ "${mode}" = "tls" ]; then
        start_tls_server "${server_bin}" "${LOG_FILE}" "${PID_FILE}"
    elif [ "${lang}" = "python" ]; then
        start_server "${server_bin}" "${LOG_FILE}" "${PID_FILE}" "--fresh"
    else
        start_server "${server_bin}" "${LOG_FILE}" "${PID_FILE}"
    fi

    wait_for_server "${LOG_FILE}" "${SERVER_TIMEOUT}" || return 1

    "run_${lang}_examples" "${mode}" || exit_code=$?

    stop_server "${PID_FILE}"
    report_test_results "${exit_code}" "${LOG_FILE}" "${PID_FILE}"

    return "${exit_code}"
}

# ===========================================================================
# Main
# ===========================================================================
LANGUAGES=()
if [ "${LANGUAGE}" = "all" ]; then
    LANGUAGES=(rust node go python java csharp)
else
    LANGUAGES=("${LANGUAGE}")
fi

FINAL_EXIT_CODE=0

for lang in "${LANGUAGES[@]}"; do
    if [ "${TLS_ONLY}" = false ]; then
        run_language "${lang}" "plain" || FINAL_EXIT_CODE=$?
    fi
    if [ "${SKIP_TLS}" = false ]; then
        run_language "${lang}" "tls" || FINAL_EXIT_CODE=$?
    fi
done

exit "${FINAL_EXIT_CODE}"
