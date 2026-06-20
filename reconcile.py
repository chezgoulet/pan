#!/usr/bin/env python3
import subprocess, json

tok = open('/opt/data/.github_token').read().strip()
base = 'https://api.github.com/repos/chezgoulet/pan'

for num in [6, 7, 8, 16, 9, 25, 26, 32, 34, 35, 42, 43, 44]:
    req = subprocess.run(
        ['curl', '-s', '-H', 'Authorization: Bearer ' + tok,
         '-H', 'Accept: application/vnd.github.v3+json',
         base + '/issues/' + str(num)],
        capture_output=True, text=True
    )
    data = json.loads(req.stdout)
    title = data.get('title', 'ERROR')
    labels = [l['name'] for l in data.get('labels', [])]
    body = data.get('body', '')
    print('#' + str(num) + ': ' + title)
    print('  Labels: ' + str(labels))
    print('  Body: ' + body[:200].strip() + '...')
    print()
