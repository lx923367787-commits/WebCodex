"""Keep the local MCP host alive while testing its asynchronous pilot tools."""
import asyncio
import json
from pathlib import Path
import sys

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

ROOT = Path(__file__).resolve().parent


def unpack(result):
    if result.isError:
        raise RuntimeError("pilot_tool_error")
    return json.loads(next(item.text for item in result.content if item.type == "text"))


async def main():
    config = json.loads((ROOT / "pilot-request.json").read_text(encoding="utf-8"))
    arguments = {"request_id": config["launch_id"]}
    trace = {"request_id": config["launch_id"]}
    path = ROOT / ("mcp-run-" + config["launch_id"] + ".json")
    async with stdio_client(StdioServerParameters(command=sys.executable,
            args=[str(ROOT / "probe_tools.py")])) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            trace["tools"] = [item.name for item in (await session.list_tools()).tools]
            trace["launch"] = unpack(await session.call_tool("spawn_chat_task", arguments))
            trace["repeat"] = unpack(await session.call_tool("spawn_chat_task", arguments))
            trace["same_worker_on_repeat"] = (trace["launch"].get("worker_pid") is not None
                and trace["launch"].get("worker_pid") == trace["repeat"].get("worker_pid"))
            path.write_text(json.dumps(trace, ensure_ascii=False, indent=2), encoding="utf-8")
            print(json.dumps({"launched": trace["launch"], "same_worker_on_repeat":
                trace["same_worker_on_repeat"]}, ensure_ascii=False), flush=True)
            deadline = asyncio.get_running_loop().time() + 300
            while asyncio.get_running_loop().time() < deadline:
                trace["last_observation"] = unpack(await session.call_tool("read_chat_task", arguments))
                if "worker_outcome" in trace["last_observation"]:
                    break
                await asyncio.sleep(5)
            path.write_text(json.dumps(trace, ensure_ascii=False, indent=2), encoding="utf-8")
            print(json.dumps(trace["last_observation"], ensure_ascii=False), flush=True)


if __name__ == "__main__":
    asyncio.run(main())
