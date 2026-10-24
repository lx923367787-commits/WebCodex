import { useEffect, useState, type CSSProperties } from "react";
import residentAtlas from "../../assets/office-residents-v2.png";

export interface SceneMember {
  id: string;
  name: string;
  role: string;
  state: string;
  task: string;
  parentId: string | null;
  working: boolean;
  kind: "manager" | "agent" | "cli";
}

// Each square atlas cell is 362 px. Per-row offsets center the original
// figures without changing the transparent source image.
const RESIDENT_OFFSETS = [
  [-15.19, 0.28, 11.19],
  [-16.99, -0.14, 10.22],
  [-14.36, 0.41, 10.08],
  [-15.88, -0.14, 7.73],
];

function residentIndex(id: string): number {
  let hash = 0;
  for (const character of id) hash = (Math.imul(hash, 31) + character.charCodeAt(0)) >>> 0;
  return hash % RESIDENT_OFFSETS.length;
}

function ResidentSprite({ residentId, animate = false }: { residentId: string; animate?: boolean }) {
  const index = residentIndex(residentId);
  const [standing, left, right] = RESIDENT_OFFSETS[index];
  const style = {
    "--ao-resident-image": 'url("' + residentAtlas + '")',
    "--ao-resident-row": (index * 100 / 3) + "%",
    "--ao-standing-shift": standing + "%",
    "--ao-left-shift": left + "%",
    "--ao-right-shift": right + "%",
  } as CSSProperties;
  return <span className={"ao-citizen-figure ao-resident-sprite " + (animate ? "ao-resident-animated" : "")} style={style} data-resident-index={index} aria-hidden="true">
    <span className="ao-resident-standing" />
    {animate && <span className="ao-resident-walking"><span className="ao-resident-stride" /></span>}
  </span>;
}

function DeskParts({ cli = false }: { cli?: boolean }) {
  return <>
    <ellipse cx="80" cy="106" rx="61" ry="7" fill="#263e2f" opacity=".08" />
    <rect x="59" y="34" width="44" height="49" rx="14" fill="#bdcbae" /><rect x="64" y="38" width="34" height="29" rx="10" fill="#d5dfc5" />
    <path d="M26 79h108v14H26z" fill="#c9a77b" /><rect x="19" y="70" width="122" height="14" rx="7" fill="#ecd1a7" /><path d="M31 93v13m98-13v13" stroke="#aa8c64" strokeWidth="5" />
    <rect x="53" y="45" width="56" height="32" rx="4" fill="#6b8375" /><rect x="58" y="49" width="46" height="23" rx="2" fill="#dce8ca" />
    {cli ? <path d="m65 55 5 4-5 4m10 0h8" stroke="#6a956a" strokeWidth="2" fill="none" /> : <path d="M64 55h16m-16 5h29m-29 5h21" stroke="#91af7b" strokeWidth="2" />}
    <path d="M52 78h59" stroke="#536d59" strokeWidth="3" strokeLinecap="round" />
    <rect x="30" y="61" width="11" height="12" rx="4" fill="#fff8e9" /><path d="M41 64q8-1 2 6" stroke="#fff8e9" strokeWidth="3" fill="none" />
    <rect x="118" y="65" width="12" height="7" rx="2" fill="#f6f0dc" />
  </>;
}

export function OfficeGlyph({ kind, residentId }: { kind: "manager" | "agent" | "cli" | "owner"; color?: string; working?: boolean; residentId?: string }) {
  return <div className="ao-desk-illustration ao-resident-portrait" aria-hidden="true">
    <ResidentSprite residentId={residentId ?? (kind === "owner" ? "owner-avatar" : kind)} />
  </div>;
}

function Plant({ className = "" }: { className?: string }) {
  return <svg className={"ao-plant " + className} viewBox="0 0 50 76" aria-hidden="true"><ellipse cx="25" cy="69" rx="19" ry="5" fill="#bac0aa" opacity=".4" /><path d="M14 50h23l-4 20H18z" fill="#bc936f" /><path d="M25 53V16" stroke="#4d6d49" strokeWidth="3" /><path d="M25 37Q2 41 4 20q20-2 21 17m0-11Q17 8 31 3q11 16-6 23m1 20q-2-24 21-25 2 23-21 25" fill="#789363" /><path d="M25 51Q7 52 8 36q16-2 17 15m1-19q1-15 16-14 0 13-16 14" fill="#4e7551" /></svg>;
}

export function OfficeScene({ members, selectedId, onSelect, demo }: { members: SceneMember[]; selectedId: string | null; onSelect: (id: string) => void; demo: boolean }) {
  const [paused, setPaused] = useState(false);
  const [reducedMotion, setReducedMotion] = useState(false);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    setReducedMotion(media.matches);
    const update = () => setReducedMotion(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  const managers = members.filter((member) => member.kind === "manager");
  const workers = members.filter((member) => member.kind !== "manager");
  const seats = [
    ...managers.map((member, index) => ({ member, x: 25 + index * 25, y: 210, manager: true })),
    ...workers.map((member, index) => ({ member, x: 21 + (index % 3) * 29, y: 370 + Math.floor(index / 3) * 180, manager: false })),
  ];
  const height = Math.max(550, 350 + Math.ceil(workers.length / 3) * 180);
  const isPaused = paused || reducedMotion;
  return <section className={"ao-scene ao-walking-scene " + (isPaused ? "is-paused" : "")} aria-label="AI 办公室成员" data-testid="office-scene">
    <div className="ao-walk-toolbar"><div><span className="ao-small-kicker">LITTLE OFFICE, BIG IDEAS</span><strong>今天也在一起，认真忙碌。</strong></div><button type="button" className="ao-motion-toggle" onClick={() => setPaused(!paused)} disabled={reducedMotion} aria-pressed={isPaused}>{reducedMotion ? "系统已减少动态" : paused ? "▶ 继续走动" : "Ⅱ 暂停走动"}</button></div>
    <div className="ao-walk-stage" style={{ height }} data-testid="walking-stage">
      <div className="ao-room-header"><div className="ao-office-sign"><span>W</span><div>WEBCODEX<small>OUR LITTLE HAPPY PLACE.</small></div></div><div className="ao-windows"><i /><i /><i /></div><div className="ao-wall-clock"><i /></div></div>
      <div className="ao-room-rug ao-room-rug-manager" />
      <div className="ao-room-owner"><OfficeGlyph kind="owner" color="#d7b46f" /><span>你 · 大老板</span></div>
      <div className="ao-room-coffee" aria-hidden="true"><svg viewBox="0 0 90 90"><ellipse cx="44" cy="81" rx="38" ry="6" fill="#cad2bb" /><path d="M10 51h70v25H10z" fill="#c5ad88" /><path d="M10 49h70v9H10z" fill="#e5cba1" /><rect x="22" y="14" width="37" height="38" rx="5" fill="#92a78d" /><rect x="28" y="20" width="25" height="20" rx="3" fill="#4e6b54" /><path d="M39 28v9" stroke="#dccca6" strokeWidth="4" /><rect x="33" y="40" width="13" height="11" rx="3" fill="#fff3d4" /><rect x="66" y="38" width="9" height="13" rx="3" fill="#f6eccf" /><path d="M14 76v7m62-7v7" stroke="#aa9474" strokeWidth="4" /></svg><span>补充一点灵感 ☕</span></div>
      <div className="ao-room-chat" aria-hidden="true"><div className="ao-room-sofa"><i /><i /></div><span>灵感碰头角</span></div>
      <Plant className="ao-walk-plant-one" /><Plant className="ao-walk-plant-two" />
      <div className="ao-room-aisle"><span>一起散个步，再回到好点子里。</span><i>· · · · · · · · · · · · · · ·</i></div>
      {seats.map(({ member, x, y }, index) => <div className="ao-fixed-desk" key={"desk-" + member.id} style={{ left: x + "%", top: y - 55 }} data-testid={"fixed-desk-" + member.id}><svg viewBox="0 0 160 116" aria-hidden="true"><DeskParts cli={member.kind === "cli"} /></svg><span>{String(index + 1).padStart(2, "0")} / {member.role}</span></div>)}
      {seats.map(({ member, x, y, manager }, index) => {
        const side = index % 2 === 0 ? 90 : 10;
        const parent = members.find((item) => item.id === member.parentId);
        const style = {
          "--ao-home-x": x + "%", "--ao-home-y": y + "px",
          "--ao-lane-y": (manager ? 270 : y + 115) + "px",
          "--ao-stop-x": side + "%", "--ao-stop-y": (side === 90 ? 267 : 260) + "px",
          "--ao-trip": (24 + index % 4 * 3) + "s", "--ao-delay": -(6 + index * 4.7) + "s",
        } as CSSProperties;
        return <button type="button" key={member.id} className={"ao-wanderer " + (selectedId === member.id ? "is-selected " : "") + (member.working ? "is-working" : "")} style={style} onClick={() => onSelect(member.id)} aria-pressed={selectedId === member.id} aria-label={"查看" + member.name + "的任务"} title={member.name + " · " + member.role + " · " + member.state + (parent ? " · 汇报给" + parent.name : "")}>
          <span className="ao-citizen-name"><i className={"ao-status-dot " + (member.working ? "" : "is-muted")} />{member.name}</span>
          <ResidentSprite residentId={member.id} animate />
          <span className="ao-citizen-bubble" aria-hidden="true">{index % 2 ? "♪" : "☕"}</span>
        </button>;
      })}
      {members.length === 0 && <div className="ao-walk-empty"><OfficeGlyph kind="agent" color="#b5c4a0" /><strong>这里安静，随时准备开工</strong><p>小伙伴入驻后，会在这里走动、碰头、回到工位。</p></div>}
      <div className="ao-room-bottom"><span>MAKE ROOM FOR GOOD IDEAS.</span><Plant /></div>
    </div>
    <div className="ao-walk-caption"><span><i className="ao-status-dot" />点击小人，看它正在忙什么</span><small>{demo ? "演示场景" : "运行时成员"} · 走动是装饰动效，不代表实际任务行为</small></div>
  </section>;
}
