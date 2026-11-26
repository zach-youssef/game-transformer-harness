"""Long native calls release the GIL, so other Python threads make progress meanwhile."""

import threading
import time

import pytest

from gtharness import _native


def test_two_threads_in_native_calls_overlap():
    seconds = 0.5
    threads = [threading.Thread(target=_native._sleep_detached, args=(seconds,)) for _ in range(2)]
    start = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    elapsed = time.perf_counter() - start
    # Holding the GIL would run the two calls one after the other: at least 2 * seconds.
    assert seconds <= elapsed < 2 * seconds


def test_python_thread_runs_during_a_native_call():
    ticks = 0
    stop = threading.Event()

    def count():
        nonlocal ticks
        while not stop.is_set():
            ticks += 1
            time.sleep(0.001)

    counter = threading.Thread(target=count)
    counter.start()
    try:
        before = ticks
        _native._sleep_detached(0.5)
        during = ticks - before
    finally:
        stop.set()
        counter.join()
    # 0.5 s at 1-16 ms per tick (Windows timers are coarse) is 30 or more ticks. With the GIL
    # held, the counter would only get a tick or two around the call.
    assert during >= 10


def test_bad_duration():
    with pytest.raises(ValueError):
        _native._sleep_detached(-1.0)
