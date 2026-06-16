---
deck: 1
type: workflow
name: PR 생성
mode: headless
lockPolicy: repo
permissionProfile:
  permissionMode: default
  allowedTools: []
steps:
- kind: cli
  command: genai-pr
  args:
  - -y
---
