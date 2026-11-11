"""Shared process accounting for native and container language measurements."""
from pathlib import Path


def process_residency(rows, bridge_pid, observed_at):
    assert bridge_pid in rows, 'Execution host exited'
    children, frontier = set(), {bridge_pid}
    while frontier:
        found = {pid for pid, (parent, _, _) in rows.items() if parent in frontier} - children - {bridge_pid}
        children.update(found)
        frontier = found

    def node(pid):
        return Path(rows[pid][2]).name == 'node'

    roots = []
    for pid in sorted(children):
        if not node(pid):
            continue
        parent, seen = rows[pid][0], {pid}
        while parent in children:
            assert parent not in seen, 'Cyclic process observation'
            seen.add(parent)
            if node(parent):
                break
            parent = rows[parent][0]
        else:
            roots.append(pid)
    return {'time': observed_at, 'bridge_bytes': rows[bridge_pid][1],
            'children_bytes': sum(rows[pid][1] for pid in children),
            'server_pids': roots,
            'children': [{'pid': pid, 'parent_pid': rows[pid][0],
                          'resident_bytes': rows[pid][1], 'program': rows[pid][2]}
                         for pid in sorted(children)]}


def service_tree(sample, server_pids, bridge_pid):
    """Include the selected servers' owning watchers and their descendants."""
    rows = {child['pid']: child for child in sample['children']}
    selected = set(server_pids)
    assert selected, 'No selected language server'
    tree = set()
    for pid in selected:
        seen = set()
        while pid != bridge_pid:
            assert pid in rows and pid not in seen, 'Incomplete process-owner observation'
            seen.add(pid)
            tree.add(pid)
            pid = rows[pid]['parent_pid']
    frontier = set(tree)
    while frontier:
        found = {pid for pid, child in rows.items() if child['parent_pid'] in frontier} - tree
        tree.update(found)
        frontier = found
    return tree
