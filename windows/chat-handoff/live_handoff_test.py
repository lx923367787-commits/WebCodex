"""One explicit live smoke test against the existing local Desktop Server.

Creates a named disposable planning Goal. It never claims or runs a business
node. The fresh web chat is asked only to verify this exact test handoff.
"""
import json
from pathlib import Path
import time
import uuid

import auto_handoff as auto
import edge_adapter
from run_auto_handoff import DEFAULT_STATE

ROOT = Path(__file__).resolve().parent
STATE = DEFAULT_STATE
EVIDENCE = ROOT / 'evidence' / 'live-handoff-20261023'


def context():
    runtime = json.loads(STATE.read_text(encoding='utf-8-sig'))['runtime']
    token = Path(runtime['user_token_file']).read_text(encoding='utf-8-sig').strip()
    return auto.Api(runtime['server_url'], token)


def save(name, value):
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    auto.write_request(EVIDENCE / name, value)


def prepare():
    if (EVIDENCE / 'test.json').exists():
        raise RuntimeError('Test already exists; resume its observation, never create a duplicate')
    api = context()
    projects = api.call('list_projects', {})['projects']
    candidates = [p for p in projects if str(p.get('root', p.get('path', ''))).replace('\\', '/').removeprefix('//?/').rstrip('/').lower() == 'e:/wbecodex']
    if len(candidates) != 1:
        raise RuntimeError('Exact test Project not found')
    project = candidates[0].get('id') or candidates[0].get('project_id')
    test_id = str(uuid.uuid4())
    epoch = 'handoff-test-source-' + test_id
    store = auto.Store(EVIDENCE / 'scheduler.sqlite3')
    store.initialize(api.origin, armed_at=int(time.time()))
    store.close()
    # Save creation identity before its one idempotent API call.
    config = {'test_id': test_id, 'source_context_epoch': epoch, 'project': project}
    save('prepared.json', config)
    output = api.call('goal_mode_start', {
        'project': project, 'task_directory': (EVIDENCE.relative_to(ROOT.parent) / 'task').as_posix(),
        'objective': '自动接续功能的一次隔离验证。新聊天恢复本 Goal 后，只调用 goal_bootstrap 与 goal_read(view=summary)核验任务ID、项目和交接状态，再回复“自动接续测试已恢复”及goal_id。不要保存计划、认领节点、执行命令或修改业务文件。',
        'idempotency_key': 'auto-handoff-test-' + test_id, 'context_epoch': epoch})
    goal = output['goal']
    config.update(goal_id=goal['goal_id'])
    save('test.json', config)
    handoff = api.call('goal_checkpoint', {'goal_id': goal['goal_id'], 'expected_revision': goal['revision'],
        'context_epoch': epoch, 'idempotency_key': 'handoff-' + test_id,
        'patch': {'action': 'handoff', 'reason': '用户已批准的一次自动接续测试',
                  'state': {'next_step': '新聊天仅恢复和读取摘要，报告测试已恢复；无需规划或业务执行。'}}})
    observed = api.read(config)
    save('source-handoff.json', observed)
    print(json.dumps({'goal_id': config['goal_id'], 'source_epoch': epoch,
                      'retired_at': observed['source_context']['retired_at'],
                      'deadline_at_ms': observed['source_context']['retired_at'] + 120000}, ensure_ascii=True), flush=True)


def run():
    config = json.loads((EVIDENCE / 'test.json').read_text(encoding='utf-8-sig'))
    live = context()
    class TestApi(auto.Api):
        def discover(self):
            return [entry for entry in super().discover() if entry['goal_id'] == config['goal_id']]
    api = TestApi(live.origin, live.token)
    store = auto.Store(EVIDENCE / 'scheduler.sqlite3')
    deadline = time.monotonic() + 720
    try:
        with auto.hc.watcher_lock(ROOT / 'runtime' / 'desktop-dispatch'):
            while time.monotonic() < deadline:
                auto.poll_once(store, api, edge_adapter.launch, request_dir=EVIDENCE / 'requests')
                entries = store.entries()
                save('scheduler.json', auto.report(store))
                if entries and entries[0]['status'] in ('resumed', 'unknown', 'blocked', 'cancelled'):
                    evidence = api.read(config)
                    save('final-handoff.json', evidence)
                    print(json.dumps({'status': entries[0]['status'], 'goal_id': config['goal_id'],
                                      'resume_started': evidence.get('resume_started'),
                                      'launch_id': entries[0]['launch_id']}, ensure_ascii=True), flush=True)
                    return 0 if entries[0]['status'] == 'resumed' else 2
                time.sleep(2)
        return 3
    finally:
        store.close()


if __name__ == '__main__':
    import sys
    if sys.argv[1] == 'prepare':
        prepare()
    elif sys.argv[1] == 'run':
        raise SystemExit(run())
    else:
        raise SystemExit('Use prepare or run')
