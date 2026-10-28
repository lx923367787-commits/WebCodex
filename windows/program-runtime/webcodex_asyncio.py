"""IOCP wakeups and anonymous subprocess pipes for the Windows program sandbox.

No ephemeral TCP allowance or global named-pipe permission is needed. This
process-local adapter keeps the native asyncio proactor and moves blocking pipe
reads/writes to its executor. It never changes the OS sandbox or credentials.
"""
import os


def install():
    if os.name != "nt":
        return
    import asyncio
    import _overlapped
    import subprocess

    if getattr(asyncio, "_webcodex_iocp", False):
        return

    class LocalProactorEventLoop(asyncio.ProactorEventLoop):
        def _make_self_pipe(self):
            class NoSignalSocket:
                def fileno(self):
                    # -1 is signal.set_wakeup_fd's documented disabled state.
                    # Runner Job control, not a console signal socket, stops
                    # this noninteractive isolated process tree.
                    return -1
            self._ssock = None
            self._csock = NoSignalSocket()
            self._wakeup_marker = _overlapped.Overlapped(0)

        def _close_self_pipe(self):
            self._ssock = self._csock = None

        def _loop_self_reading(self, future=None):
            pass

        def _write_to_self(self):
            proactor = self._proactor
            if proactor is not None:
                try:
                    _overlapped.PostQueuedCompletionStatus(proactor._iocp, 0, 0, self._wakeup_marker.address)
                except OSError:
                    if not self.is_closed():
                        raise

    class LocalPolicy(asyncio.WindowsProactorEventLoopPolicy):
        _loop_factory = LocalProactorEventLoop

    class Input:
        def __init__(self, stream):
            self.stream = stream

        def write(self, data):
            self.stream.write(data)
            self.stream.flush()

        async def drain(self):
            await asyncio.to_thread(self.stream.flush)

        def close(self):
            self.stream.close()

        def is_closing(self):
            return self.stream.closed

    class Output:
        def __init__(self, stream):
            self.stream = stream

        async def read(self, size=-1):
            return await asyncio.to_thread(self.stream.read, size)

        async def readexactly(self, size):
            data = bytearray()
            while len(data) < size:
                chunk = await self.read(size - len(data))
                if not chunk:
                    raise asyncio.IncompleteReadError(bytes(data), size)
                data.extend(chunk)
            return bytes(data)

    class Process:
        def __init__(self, process):
            self.process = process
            self.stdin = Input(process.stdin) if process.stdin else None
            self.stdout = Output(process.stdout) if process.stdout else None
            self.stderr = Output(process.stderr) if process.stderr else None

        @property
        def returncode(self):
            return self.process.poll()

        @property
        def pid(self):
            return self.process.pid

        async def wait(self):
            return await asyncio.to_thread(self.process.wait)

        async def communicate(self, input=None):
            return await asyncio.to_thread(self.process.communicate, input)

        def terminate(self):
            self.process.terminate()

        def kill(self):
            self.process.kill()

    async def create_subprocess_exec(program, *args, **options):
        options.pop("limit", None)
        return Process(subprocess.Popen([program, *args], **options))

    asyncio.set_event_loop_policy(LocalPolicy())
    asyncio.create_subprocess_exec = create_subprocess_exec
    asyncio._webcodex_iocp = True


install()
