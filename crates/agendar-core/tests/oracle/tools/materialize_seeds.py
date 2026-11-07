#!/usr/bin/env python3
"""Materialize authored spec inputs/assertions; never imports/runs a reducer.

--check compares committed fixtures byte-for-byte without writing. Assertions
below are deliberately literal. Full-State implementation goldens need separate
review; these seeds are independent targeted oracles, not full-State snapshots.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
H1 = 'h-' + '0' * 25 + '1'
H2 = 'h-' + '0' * 25 + '2'
S1, S2, S3 = ('0' * 23 + x for x in ('101', '102', '103'))
A, B, C = (f'{H1}/s-{s}' for s in (S1, S2, S3))
OP, OP2, ST = f'{H1}/operator', f'{H2}/operator', f'{H1}/steward'
AS_OF = '2026-01-01T12:00:00.000Z'


def uid(n):
    return str(n).zfill(26)


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')) + '\n'


def agent(writer, harness='generic'):
    return {'session': writer.split('/s-')[1], 'harness': harness}


def policy(mode='ask'):
    return {'requested': mode, 'effective': mode, 'reversibility': 'easy',
            'external': False, 'heuristic_hits': [],
            'budget': {'speculative_features': 0, 'pending_slices': 0,
                       'demo_gap_s': 0, 'blocked': False, 'reasons': []}}


def card(assume=False, byte_criterion=False):
    value = json.loads((ROOT / 'examples/ask.json').read_text())
    if assume:
        value['reach'] = 'wide'
    if byte_criterion:
        value['done_when'] = [{'id': 'bytes', 'text': 'Demo bytes match the approved digest.'}]
    return value


class Seed:
    def __init__(self, name):
        self.name, self.events, self.counts, self.aliases = name, [], {}, {}
        self.expected, self.extra_files = {}, {}

    def add(self, alias, writer, kind, body, refs=None, minute=0):
        self.counts[writer] = self.counts.get(writer, 0) + 1
        event_id = uid(len(self.events) + 1)
        self.aliases[alias] = event_id
        self.events.append({'v': 1, 'id': event_id, 'seq': self.counts[writer],
                            'writer': writer, 'ts': f'2026-01-01T{minute // 60:02}:{minute % 60:02}:00.000Z',
                            'kind': kind, 'project': 'example', 'refs': refs or {}, 'body': body})
        return event_id

    def setup(self, nodes):
        self.add('project', OP, 'project.declared',
                 {'title': 'Example', 'brief': 'Spec fixture', 'goal': 'Verify the contract'})
        for node, deps in nodes:
            self.add('plan-' + node, OP, 'plan.feature',
                     {'title': node, 'slice': 'example', 'demo_kind': 'cli_transcript', 'depends_on': deps},
                     {'node': node})

    def start(self, alias, node, writer=A, minute=1, causes=None, basis=None, harness='generic'):
        refs = {'node': node}
        if causes:
            refs['causes'] = causes
        if basis:
            refs['basis'] = basis
        return self.add(alias, writer, 'work.progress',
                        {'agent': agent(writer, harness), 'status': 'started',
                         'eta': {'min_s': 600, 'max_s': 600}, 'evidence': []}, refs, minute)

    def ask(self, alias='ask', node='x', writer=A, minute=2, assume=False, byte_criterion=False):
        return self.add(alias, writer, 'decision.ask',
                        {'agent': agent(writer), 'card': card(assume, byte_criterion),
                         'policy': policy('assume' if assume else 'ask')}, {'node': node}, minute)

    def answer(self, alias, ask, option='adapters', writer=OP, minute=3, causes=None, byte_criterion=False):
        refs = {'node': 'x', 'ask': ask}
        if causes:
            refs['causes'] = causes
        return self.add(alias, writer, 'decision.answer',
                        {'option': option, 'done_when': card(byte_criterion=byte_criterion)['done_when']},
                        refs, minute)


seeds = []
s = Seed('answer-conflict')
s.setup([('x', [])])
q = s.ask()
a = s.answer('a', q, minute=3)
b = s.answer('b', q, option='inbox', writer=OP2, minute=4)
s.expected = {'decision': {'ask': q, 'effective_receipt': b, 'applied_answer': None,
                           'lifecycle': 'answered', 'answer_history': [a, b]},
              'pending_pivot': False}
seeds.append(s)

s = Seed('plan-cycle')
s.setup([('x', []), ('y', ['x'])])
p = s.add('closing-patch', OP, 'plan.feature', {'depends_on': ['y']}, {'node': 'x'}, 1)
s.expected = {'accepted_edges': [['x', 'y']], 'rejected_edges': [['y', 'x']],
              'rescue_ids': [f'rescue:cycle:{p}:y:x'], 'source': p}
seeds.append(s)

s = Seed('delivery-seen-review')
s.setup([('x', []), ('y', ['x'])])
attempt = s.start('attempt', 'x')
q = s.ask(byte_criterion=True)
r = s.answer('receipt', q, byte_criterion=True)
blob_bytes = b'fixture check output\n'
digest = hashlib.sha256(blob_bytes).hexdigest()
blob = {'host': H1, 'sha256': digest, 'ext': 'txt', 'bytes': len(blob_bytes)}
s.extra_files['blobs/' + digest + '.txt'] = blob_bytes.decode()
d = s.add('delivery', A, 'delivery', {'agent': agent(A), 'name': 'Example bytes',
          'demos': [{'kind': 'cli_transcript', 'body': {'blob': blob}}],
          'criteria': [{'criterion': 'bytes', 'status': 'passed', 'claim': 'agent_says', 'evidence': blob}],
          'disclosure': {'proven': ['Agent compared bytes'], 'not_proven_yet': ['Independent check'], 'faked_for_now': []}},
          {'node': 'x', 'attempt': attempt, 'receipt': r}, 4)
concurrent = copy.deepcopy(s)
concurrent.name = 'concurrent-observations'
observations = []
for i, result in enumerate(['failed', 'passed', 'failed']):
    o = s.add('observation-' + str(i), ST, 'evidence.observed',
              {'criterion': 'bytes', 'method': {'kind': 'command', 'check_id': 'fixture-equality', 'argv_digest': digest},
               'started_at': f'2026-01-01T00:0{5+i}:00.000Z',
               'finished_at': f'2026-01-01T00:0{5+i}:01.000Z', 'result': result,
               'digest': digest, 'output': blob, 'approval_id': 'fixture-byte-check'},
              {'node': 'x', 'delivery': d, 'causes': observations[-1:]}, 5+i)
    observations.append(o)
review_a = s.add('accept', OP, 'review.result', {'verdict': 'accept'},
                 {'node': 'x', 'delivery': d, 'causes': [r, observations[-1]]}, 8)
review_b = s.add('send-back', OP2, 'review.result', {'verdict': 'send_back', 'reason': 'Check disagrees'},
                 {'node': 'x', 'delivery': d, 'causes': [r, observations[-1]]}, 9)
s.add('rule', OP2, 'rule.set', {'rule_id': 'check-bytes', 'text': 'Compare bytes before delivery', 'enabled': True},
      {'causes': [review_b]}, 10)
review_c = s.add('rereview', OP, 'review.result', {'verdict': 'accept'},
                 {'node': 'x', 'delivery': d, 'causes': [review_a, review_b, observations[-1], r]}, 11)
s.expected = {'delivery': d, 'agent_result': 'passed', 'seen_result': 'failed',
              'observation': observations[-1], 'conflict': True,
              'failure_streak': 1, 'failure_rescue': False,
              'review': 'accepted', 'review_event': review_c, 'review_frontier': [review_c],
              'review_history': [review_a, review_b, review_c], 'enabled_rules': ['check-bytes'],
              'prefixes': [{'through': observations[0], 'seen_result': 'failed', 'review': 'pending'},
                           {'through': observations[1], 'seen_result': 'passed'},
                           {'through': review_b, 'review': 'sent_back', 'review_conflict': True,
                            'downstream_review_gate': 'blocked'}],
              'delivery_event_unchanged': True}
invalid = copy.deepcopy(next(e for e in s.events if e['id'] == d))
invalid['body']['criteria'][0]['claim'] = 'seen'
invalid['body']['criteria'][0]['observation'] = observations[0]
invalid['refs']['causes'] = [observations[0]]
s.extra_files['invalid-delivery.json'] = canonical(invalid)
s.expected['invalid_variant'] = {'code': 'E_VALIDATION', 'detail': 'delivery_seen_requires_later_observation',
                                  'field': '/criteria/0/claim', 'not': 'causal_cycle'}
seeds.append(s)

s = concurrent
frontier = []
for i, (writer, result) in enumerate([(ST, 'passed'), (f'{H2}/steward', 'failed')]):
    output = dict(blob, host=writer.split('/')[0])
    frontier.append(s.add('concurrent-' + str(i), writer, 'evidence.observed',
        {'criterion': 'bytes', 'method': {'kind': 'command', 'check_id': 'fixture-equality', 'argv_digest': digest},
         'started_at': '2026-01-01T00:05:00.000Z', 'finished_at': '2026-01-01T00:05:01.000Z',
         'result': result, 'digest': digest, 'output': output, 'approval_id': 'fixture-check'},
        {'node': 'x', 'delivery': d}, 5))
s.expected = {'delivery': d, 'observation_frontier': frontier,
              'observation': frontier[1], 'seen_result': 'failed', 'conflict': True,
              'failure_streak': 1, 'failure_rescue': False}
seeds.append(s)

s = Seed('concurrent-consumption')
s.setup([('x', []), ('y', ['x'])])
a = s.start('attempt-a', 'x', minute=1)
b = s.start('attempt-b', 'x', writer=B, minute=2)
q = s.ask(minute=3, assume=True)
choice = s.add('choice', A, 'decision.assumed',
               {'agent': agent(A), 'option': 'adapters', 'reason': 'policy', 'policy': policy('assume')},
               {'node': 'x', 'ask': q, 'attempt': a}, 4)
s.answer('override', q, option='inbox', minute=5, causes=[choice])
s.expected = {'current_attempt_x': b, 'current_assumptions_x': [], 'current_taints_x': [],
              'built_on': [], 'redo': [], 'historical_consumption': {a: [q]},
              'start_conflict_retained': True}
seeds.append(s)

s = Seed('transitive-taint')
s.setup([('x', []), ('y', ['x']), ('z', [])])
a = s.start('attempt-x', 'x', minute=1)
z = s.start('unrelated-z', 'z', writer=C, minute=1)
q = s.ask(minute=2, assume=True)
choice = s.add('choice', A, 'decision.assumed',
               {'agent': agent(A), 'option': 'adapters', 'reason': 'policy', 'policy': policy('assume')},
               {'node': 'x', 'ask': q, 'attempt': a}, 3)
done = s.add('done-x', A, 'work.progress', {'agent': agent(A), 'status': 'done', 'evidence': []},
            {'node': 'x', 'attempt': a}, 4)
y = s.start('attempt-y', 'y', writer=B, minute=5, causes=[done], basis=[choice])
s.answer('override', q, option='inbox', minute=6, causes=[choice, y])
s.expected = {'built_on': ['x', 'y'], 'redo': ['x', 'y'],
              'taints': {'x': [q], 'y': [q], 'z': []},
              'progress': {'x': 'done', 'y': 'doing', 'z': 'doing'},
              'basis': {'x': 'tainted', 'y': 'tainted', 'z': 'firm'}}
seeds.append(s)

s = Seed('ranking-two-asks')
s.setup([('x', []), ('y', [])])
s.start('attempt-x', 'x', minute=100)
s.start('attempt-y', 'y', writer=B, minute=101)
qa = s.ask('ask-a', minute=710)
qb = s.ask('ask-b', minute=710)
s.expected = {'runnable_for_session': {S1: [], S2: ['y']}, 'should_wait': {S1: True, S2: False},
              'needs': [{'id': 'decide:' + qa, 'stalled_agents': 1, 'waiting_minutes': 10,
                         'weight_x2': 2, 'score_x2': 20, 'notification_eligible': True},
                        {'id': 'decide:' + qb, 'stalled_agents': 1, 'waiting_minutes': 10,
                         'weight_x2': 2, 'score_x2': 20, 'notification_eligible': True}],
              'fleet_stalled': 1}
seeds.append(s)

for name, distinct in [('eta-one-feature-five-attempts', False), ('eta-five-features-mixed', True)]:
    s = Seed(name)
    s.setup([(f'f{i}', []) for i in range(5)] if distinct else [('f0', [])])
    previous = None
    durations = [5, 10, 15, 20, 25]  # literal actual minutes, estimate10m
    for i, duration in enumerate(durations):
        node = f'f{i}' if distinct else 'f0'
        writer = B if distinct and i >= 3 else A
        harness = 'codex' if writer == B else 'generic'
        start = s.start('start-' + str(i), node, writer=writer, minute=i * 60,
                        causes=[previous] if previous and not distinct else None, harness=harness)
        s.add('done-' + str(i), writer, 'work.progress',
              {'agent': agent(writer, harness), 'status': 'done', 'evidence': []},
              {'node': node, 'attempt': start}, i * 60 + duration)
        previous = start
    s.expected = {'finished_feature_count': 5 if distinct else 1,
                  'attempt_samples': 5, 'eligible': distinct,
                  # Eligible project, but neither harness has the five timed samples a ratio
                  # needs (generic 3, codex 2), so nothing is calibrated yet.
                  'calibration': []}
    seeds.append(s)

s = Seed('eta-overlapping-pauses')
s.setup([('x', [])])
a = s.start('attempt', 'x', minute=0)
q1 = s.ask('q1', minute=2)
q2 = s.ask('q2', minute=4)
s.answer('r1', q1, minute=6)
s.answer('r2', q2, minute=8)
s.add('done', A, 'work.progress', {'agent': agent(A), 'status': 'done', 'evidence': []},
      {'node': 'x', 'attempt': a}, 16)
s.expected = {'pause_intervals_minutes': [[2, 6], [4, 8]], 'pause_union_minutes': [[2, 8]],
              'paused_s': 360, 'actual_active_s': 600, 'sample_ratio_milli': 1000,
              'eligible': False, 'calibration': []}
seeds.append(s)

parser = argparse.ArgumentParser()
parser.add_argument('--check', action='store_true')
args = parser.parse_args()
outputs = {}
for s in seeds:
    base = ROOT / 'seeds' / s.name
    outputs[base / 'events.jsonl'] = ''.join(canonical(e) for e in s.events)
    outputs[base / 'context.json'] = canonical({'as_of': AS_OF})
    outputs[base / 'expected.json'] = canonical({'oracle_version': 1, 'assertions': s.expected})
    outputs[base / 'aliases.json'] = canonical(s.aliases)
    for name, content in s.extra_files.items():
        outputs[base / name] = content
for path, content in outputs.items():
    if args.check:
        if not path.exists() or path.read_text() != content:
            raise SystemExit(f'Spec seed differs: {path.relative_to(ROOT)}')
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
print(f'{"Verified" if args.check else "Wrote"} {len(outputs)} files across {len(seeds)} authored seeds; no reducer executed.')
