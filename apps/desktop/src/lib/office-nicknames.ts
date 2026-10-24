const STORAGE_KEY = "webcodex.office.demo-nicknames";
const IDS = ["manager", "frontend", "backend", "review", "cli-workbuddy", "cli-antigravity", "docs-manager"];
const NAMES = ["糯米", "桃桃", "奶盖", "豆包", "布丁", "泡芙", "啵啵", "栗子", "软糖", "小团子", "棉棉", "圆圆", "橘宝", "小麻薯", "米粒", "花卷"];
let aliases: Record<string, string> | undefined;

/** A display nickname for a real task; no storage, randomness or identity changes. */
export function nicknameForTask(id: string): string {
  let hash = 0;
  for (const character of id) hash = (Math.imul(hash, 31) + (character.codePointAt(0) ?? 0)) >>> 0;
  return NAMES[hash % NAMES.length];
}

/** Give the demo cast random nicknames once; polling and reloads keep their identity. */
export function demoNickname(id: string): string {
  if (!aliases) {
    try {
      const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null");
      if (saved && IDS.every((key) => NAMES.includes(saved[key])) && new Set(IDS.map((key) => saved[key])).size === IDS.length) aliases = saved;
    } catch { /* Nicknames remain available when browser storage is unavailable. */ }
    if (!aliases) {
      const available = [...NAMES];
      aliases = Object.fromEntries(IDS.map((key) => [key, available.splice(Math.floor(Math.random() * available.length), 1)[0]]));
      try { localStorage.setItem(STORAGE_KEY, JSON.stringify(aliases)); } catch { /* Keep this page's cast. */ }
    }
  }
  return aliases[id] ?? "小伙伴";
}
