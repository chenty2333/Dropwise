#!/usr/bin/env python3
"""Build pinned variants, then run the frozen 72-row comparison sequentially."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
PARENT = 'ad8c59add6a1988d8c327fb3358beeeae3bbb5cd'
FIX = 'c79121391db8f8d36d4213feeb25381caee110c7'
MODES = ['A', 'B', 'B-settle', 'C-matched', 'C-full', 'C-no-settle']

def prepare(work):
    builds = {}
    for variant, revision in [('parent', PARENT), ('fixed', FIX)]:
        directory = work / variant
        (directory / 'src').mkdir(parents=True, exist_ok=True)
        (directory / '.cargo').mkdir(exist_ok=True)
        manifest = (HERE / 'Cargo.toml').read_text().replace('path = "../.."', f'path = "{ROOT}"')
        (directory / 'Cargo.toml').write_text(manifest.replace(PARENT, revision))
        shutil.copyfile(HERE / 'src/main.rs', directory / 'src/main.rs')
        shutil.copyfile(HERE / '.cargo/config.toml', directory / '.cargo/config.toml')
        shutil.copyfile(HERE / ('Cargo.lock' if variant == 'parent' else 'Cargo.fixed.lock'), directory / 'Cargo.lock')
        start = time.monotonic()
        result = subprocess.run(['timeout', '900s', 'cargo', 'build', '--locked', '--offline'], cwd=directory,
                                env=dict(os.environ, CARGO_BUILD_JOBS='2'))
        builds[variant] = {'ok': result.returncode == 0, 'seconds': time.monotonic()-start}
    print('builds:', json.dumps(builds), flush=True)
    return builds

def command(work, case, variant, mode):
    dependency = 'fixed' if case == 'U' and variant == 'fixed' else 'parent'
    return [str(work / dependency / 'target/debug/dropwise-mechanisms'), case, variant, mode, str(work / 'input.txt')]

def backend(work, variant):
    if not shutil.which('strace'):
        return False
    run = subprocess.run(['timeout', '20s', 'strace', '-e', 'trace=io_uring_setup,io_uring_enter',
                          *command(work, 'U', variant, 'A')], capture_output=True, text=True)
    return run.returncode == 0 and bool(re.search(r'io_uring_setup\(.*\)\s+=\s+\d+', run.stderr)) and 'io_uring_enter(' in run.stderr

def measure(work, case, variant, mode, repeat):
    start = time.monotonic()
    result = subprocess.run(['timeout', '20s', *command(work, case, variant, mode)], capture_output=True, text=True)
    row = {'case': case, 'variant': variant, 'mode': mode, 'repeat': repeat, 'process_seconds': time.monotonic()-start}
    if result.returncode:
        return row | {'status': 'execution_error', 'exit_code': result.returncode, 'error': result.stderr[-2000:]}
    try:
        metrics = json.loads(result.stdout)
    except json.JSONDecodeError:
        return row | {'status': 'harness_error', 'output': result.stdout[-2000:], 'error': result.stderr[-2000:]}
    return row | metrics | {'status': 'measured'}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, default=Path('/home/ava/dropwise-mechanism-work'))
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--output', type=Path, default=HERE / 'measurements.jsonl')
    args = parser.parse_args()
    if not args.prepare_only and args.output.exists():
        parser.error('output exists; refusing to overwrite measurements')
    builds = prepare(args.work)
    if args.prepare_only:
        return
    (args.work / 'input.txt').write_text('x')
    backends = {variant: builds['fixed' if variant == 'fixed' else 'parent']['ok'] and backend(args.work, variant)
                for variant in ['buggy', 'fixed']}
    print('io_uring backend:', backends, flush=True)
    baseline_ok = {'N': True, 'U': True}
    deadline = time.monotonic() + 1800
    with args.output.open('x') as output:
        for mode in MODES:  # all A variants/repeats precede any injection
            for case in ['N', 'U']:
                for variant in ['buggy', 'fixed']:
                    for repeat in range(1, 4):
                        dependency = 'fixed' if case == 'U' and variant == 'fixed' else 'parent'
                        reason = None
                        if not builds[dependency]['ok']: reason = 'build_unavailable'
                        elif case == 'U' and not all(backends.values()): reason = 'backend_unavailable'
                        elif mode != 'A' and not baseline_ok[case]: reason = 'baseline_blocked'
                        elif time.monotonic() >= deadline: reason = 'budget'
                        if reason:
                            row = dict(case=case, variant=variant, mode=mode, repeat=repeat, status=reason)
                        else:
                            row = measure(args.work, case, variant, mode, repeat)
                        if mode == 'A' and (row['status'] != 'measured' or row.get('violations', 0)):
                            baseline_ok[case] = False
                        output.write(json.dumps(row) + '\n')
                        output.flush()
                        print(case, variant, mode, repeat, row['status'], row.get('violations'), flush=True)

if __name__ == '__main__':
    main()
