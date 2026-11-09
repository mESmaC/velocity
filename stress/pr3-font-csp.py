from pathlib import Path
import json
import re
import sys

root = Path(__file__).resolve().parents[1]
configuration = json.loads((root / 'src-tauri/tauri.conf.json').read_text(encoding='utf-8'))
policy = configuration['app']['security']['csp']
if isinstance(policy, str):
    directives = {}
    for directive in policy.split(';'):
        parts = directive.split()
        if parts:
            directives.setdefault(parts[0], parts[1:])
else:
    directives = policy
sources = directives.get('font-src', directives.get('default-src', []))
if isinstance(sources, str):
    sources = sources.split()
allows_data = 'data:' in sources

stylesheets = list((root / 'ui/dist/assets').glob('*.css'))
if not stylesheets:
    raise SystemExit('Run pnpm -C ui build before this regression check.')
blocked = []
face_count = 0
for stylesheet in stylesheets:
    css = stylesheet.read_text(encoding='utf-8')
    for face in re.findall(r'@font-face\s*\{([^}]*)\}', css):
        urls = [url.strip('\"\'') for url in re.findall(r'url\(([^)]+)\)', face)]
        face_count += 1
        if urls and all(url.startswith('data:') for url in urls) and not allows_data:
            fields = {}
            for property_name in ('font-family', 'font-weight', 'unicode-range'):
                match = re.search(r'(?:^|;)' + property_name + r':([^;}]*)', face)
                fields[property_name] = match.group(1) if match else '(unspecified)'
            blocked.append(fields)

if not face_count:
    raise SystemExit('No bundled font faces were found in the production CSS.')
for face in blocked:
    print('BLOCKED: {font-family}, weight {font-weight}, {unicode-range}'.format(**face))
print('Production font sources:', ' '.join(sources))
if blocked:
    print(f'FAIL: {len(blocked)} bundled font faces have only data URL sources forbidden by the CSP.')
    sys.exit(1)
print(f'PASS: none of {face_count} bundled font faces depends only on a forbidden data URL.')
