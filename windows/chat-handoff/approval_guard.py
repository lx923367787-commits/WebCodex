"""Bounded WebCodex approval handling inside the owned recovery chat.

DOM is used for observation/scrolling only. Approval is always an ordinary
Browser input events target the observed button, never a DOM click.
The browser adapter owns trusted pointer input and cancellation checks.
"""
import json
import re
import time
from urllib.parse import urlsplit


USER_MESSAGES_JS = r'''
  const userCandidates = [...document.querySelectorAll('[data-user-message-bubble="true"],[data-markdown-text-tone="user-message"],[data-message-author-role="user"]')];
  const users = userCandidates.filter(e => !userCandidates.some(parent => parent !== e && parent.contains(e)));
'''


PROBE_JS = r'''function(_element, argument) {
  const args = JSON.parse(argument);
  const norm = s => (s || '').replace(/[\u200b-\u200d\ufeff]/g, '').replace(/\s+/g, ' ').trim();
  const visible = e => {
    if (!e || !e.isConnected || e.closest('[hidden],[aria-hidden="true"]')) return false;
    const r = e.getBoundingClientRect();
    if (r.width <= 1 || r.height <= 1) return false;
    for (let p = e; p; p = p.parentElement) {
      const s = getComputedStyle(p);
      if (s.display === 'none' || s.visibility !== 'visible' || Number(s.opacity) <= 0) return false;
    }
    return true;
  };
''' + USER_MESSAGES_JS + r'''
  const matchingUsers = users.filter(e => (e.textContent || '').includes(args.launch_id)
    && (e.textContent || '').includes(args.goal_id));
  const expectedCount = args.expected_user_count || 1;
  const bound = matchingUsers.length === 1 && users.length === expectedCount
    && (matchingUsers[0] === users[users.length - 1] || args.approved_prior_launch === true);
  const surfaces = [...document.querySelectorAll('[data-codex-approval-surface="true"]')].filter(visible);
  const titleMatches = s => /^(?:允许\s*ChatGPT\s*使用\s*webcodex\s*(?:吗)?|Allow\s+ChatGPT\s+to\s+use\s+webcodex)\s*[?？]?$/i.test(norm(s));
  const cards = surfaces.map(card => {
    const alert = card.querySelector('[role="alert"]');
    const text = norm(alert ? alert.textContent : '');
    const exactTitle = !!alert && [alert, ...alert.querySelectorAll('div,span,p')]
      .some(e => visible(e) && titleMatches(e.textContent));
    // Current cards show a description rather than a tool name. The user's
    // app-level Always allow authorization does not depend on that copy.
    const toolMatch = text.match(/ChatGPT 将调用 webcodex 的 (.+?) 工具[。.]/i)
      || text.match(/ChatGPT will (?:call|use) webcodex['’]s (.+?) tool\./i);
    const buttons = [...card.querySelectorAll('button')].filter(b => visible(b) && /^(?:始终允许|always allow)$/i.test(norm(b.textContent)));
    const b = buttons.length === 1 ? buttons[0] : null;
    const host = card.closest('[data-chatgpt-search-message-ids]');
    const owner = host ? host.getAttribute('data-chatgpt-search-message-ids') : '';
    const r = b ? b.getBoundingClientRect() : null;
    const center = r ? [r.x + r.width / 2, r.y + r.height / 2] : null;
    const inView = !!center && center[0] > 0 && center[1] > 0 && center[0] < innerWidth && center[1] < innerHeight;
    const hit = inView ? document.elementFromPoint(center[0], center[1]) : null;
    const clear = !!b && !!hit && (b === hit || b.contains(hit));
    const enabled = !!b && !b.matches(':disabled') && !b.closest('[aria-disabled="true"],[inert]');
    // Use the observed surface's document position, not dynamic CSS classes.
    const index = [...document.querySelectorAll('[data-codex-approval-surface="true"]')].indexOf(card) + 1;
    // Position is derived from the same observed DOM. Text normalization is
    // done above, including NBSP/zero-width characters that XPath 1.0 lacks.
    const buttonIndex = b ? [...card.querySelectorAll('button')].indexOf(b) + 1 : 0;
    const xpath = `((//*[@data-codex-approval-surface='true'])[${index}]//button)[${buttonIndex}]`;
    const tool = toolMatch ? norm(toolMatch[1]) : null;
    const signature = JSON.stringify([owner, tool, norm(b ? b.textContent : '')]);
    if (args.scroll_signature && args.scroll_signature === signature && b && !inView) {
      b.scrollIntoView({block:'center', inline:'nearest', behavior:'instant'});
    }
    return {exact_app:exactTitle, tool, owner, button_count:buttons.length,
      enabled, in_view:inView, unobscured:clear, center, signature, xpath};
  });
  const dialogs = [...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].filter(visible);
  return {url:location.href, document_visible:document.visibilityState === 'visible', bound,
    user_count:users.length, cards, modal:dialogs.length > 0};
}'''


def chat_url(value):
    try:
        u = urlsplit(value)
        if (u.scheme != 'https' or u.hostname != 'chatgpt.com' or u.username or u.password
                or u.port not in (None, 443) or not re.fullmatch(r'/(?:g/[^/]+/)?c/[A-Za-z0-9-]{8,}', u.path)):
            return None
        return 'https://chatgpt.com' + u.path
    except (TypeError, ValueError):
        return None


def inspect(browser, request, scroll_signature=None, *, approved_prior_launch=False):
    count = request.get('expected_user_count', 1)
    if type(count) is not int or not 1 <= count <= 10000:
        raise ValueError('invalid_expected_message_count')
    data = browser.execute_javascript(PROBE_JS, json.dumps({
        'launch_id': request['launch_id'], 'goal_id': request['goal_id'],
        'expected_user_count': count,
        'approved_prior_launch': approved_prior_launch is True,
        'scroll_signature': scroll_signature}, ensure_ascii=False))
    if isinstance(data, str):
        data = json.loads(data)
    if not isinstance(data, dict) or not isinstance(data.get('cards'), list):
        raise ValueError('invalid_browser_observation')
    return data


def candidate(snapshot):
    """No action on another app, overlapping cards, missing context, or a modal."""
    if not snapshot.get('bound') or not snapshot.get('document_visible'):
        return None, 'conversation_not_bound'
    if snapshot.get('modal'):
        return None, 'unexpected_modal'
    cards = snapshot['cards']
    if not cards:
        return None, None
    if len(cards) != 1:
        return None, 'ambiguous_approval_cards'
    card = cards[0]
    if not card.get('exact_app'):
        return None, 'unrecognized_approval'
    if card.get('button_count') != 1 or not card.get('owner'):
        return None, 'ambiguous_approval_button'
    if not card.get('enabled'):
        return None, 'button_disabled'
    if not card.get('in_view'):
        return card, 'scroll_required'
    if not card.get('unobscured'):
        return None, 'button_obscured'
    return card, None


def same_position(a, b):
    return (a.get('signature') == b.get('signature') and a.get('xpath') == b.get('xpath')
            and len(a.get('center') or []) == 2 and len(b.get('center') or []) == 2
            and all(abs(x - y) <= 2 for x, y in zip(a['center'], b['center'])))


def run(browser, request, should_stop=lambda: False, timeout=180, clock=time.monotonic, sleep=time.sleep, *, approved_prior_launch=False):
    """Observe one exact newly sent chat for a bounded approval window.

    sent means the recovery message was observed, never Goal/business success.
    A click with an unknown outcome is not repeated. Timeout never renews.
    """
    started = clock()
    deadline = started + min(max(float(timeout), 1), 300)
    target_url = None
    confirmed = []
    attempted = set()
    pending = None
    previous = None
    last_reason = 'message_not_observed'
    misses = 0

    def observe(scroll_signature=None):
        if approved_prior_launch is True:
            return inspect(browser, request, scroll_signature=scroll_signature, approved_prior_launch=True)
        return inspect(browser, request, scroll_signature=scroll_signature)

    def result(status, reason):
        return {'status': status, 'reason': reason, 'target_url': target_url,
                'approved_tools': confirmed, 'approval_click_count': len(attempted),
                'message_observed': target_url is not None, 'business_verified': False}

    while clock() < deadline:
        if should_stop():
            return result('unknown', 'cancelled_after_send')
        try:
            snapshot = observe()
        except Exception:
            misses += 1
            if misses >= 3:
                return result('unknown', 'browser_observation_failed')
            sleep(min(0.5, max(0, deadline - clock())))
            continue
        misses = 0
        url = chat_url(snapshot.get('url'))
        if target_url and url != target_url:
            return result('blocked', 'target_conversation_changed')
        if url and snapshot.get('bound'):
            target_url = url
        elif target_url:
            return result('blocked', 'target_message_changed')
        else:
            last_reason = 'message_not_observed'
            sleep(min(0.5, max(0, deadline - clock())))
            continue

        card, reason = candidate(snapshot)
        last_reason = reason
        if pending:
            current_signatures = [c.get('signature') for c in snapshot['cards']]
            if pending['signature'] not in current_signatures:
                if pending.get('tool'):
                    confirmed.append(pending['tool'])
                pending = None
                previous = None
            elif clock() - pending['clicked_at'] >= 15:
                return result('unknown', 'approval_click_unconfirmed')
            else:
                sleep(min(0.5, max(0, deadline - clock())))
                continue
        if reason in ('unexpected_modal', 'ambiguous_approval_cards', 'unrecognized_approval', 'ambiguous_approval_button'):
            return result('blocked', reason)
        if card and reason == 'scroll_required':
            if should_stop():
                return result('unknown', 'cancelled_after_send')
            try:
                observe(scroll_signature=card['signature'])
            except Exception:
                return result('unknown', 'approval_scroll_failed')
            previous = None
        elif card and reason is None:
            if card['signature'] in attempted:
                return result('unknown', 'approval_reappeared')
            if previous and same_position(previous, card):
                if len(attempted) >= 12:
                    return result('blocked', 'approval_limit_reached')
                # Refresh the exact browser, page, card and hit target again just before input.
                try:
                    fresh = observe()
                except Exception:
                    return result('unknown', 'browser_observation_failed')
                fresh_card, fresh_reason = candidate(fresh)
                if (chat_url(browser.get_url()) != target_url or chat_url(fresh.get('url')) != target_url
                        or fresh_reason is not None or not fresh_card or not same_position(card, fresh_card)):
                    previous = None
                    sleep(min(0.5, max(0, deadline - clock())))
                    continue
                try:
                    elements = browser.find_all_by_xpath(fresh_card['xpath'], timeout=1)
                    # Use the freshly observed DOM predicate, including
                    # ancestors and hit testing, rather than a stale property.
                    elements = [e for e in elements if e.is_displayed()]
                except Exception:
                    return result('unknown', 'button_resolution_failed')
                if len(elements) != 1:
                    return result('blocked', 'button_identity_changed')
                try:
                    after_resolution = observe()
                except Exception:
                    return result('unknown', 'browser_observation_failed')
                resolved_card, resolved_reason = candidate(after_resolution)
                if (chat_url(after_resolution.get('url')) != target_url or resolved_reason is not None
                        or not resolved_card or not same_position(fresh_card, resolved_card)):
                    previous = None
                    sleep(min(0.5, max(0, deadline - clock())))
                    continue
                if should_stop() or clock() >= deadline:
                    return result('unknown', 'cancelled_before_approval')
                attempted.add(card['signature'])
                pending = dict(card, clicked_at=clock())
                try:
                    elements[0].click(button='left', simulative=True, move_mouse=True, delay_after=0,
                        before_actual=lambda: not should_stop() and clock() < deadline)
                except Exception:
                    return result('unknown', 'approval_click_outcome_unknown')
                previous = None
            else:
                previous = card
        else:
            previous = None
        sleep(min(0.5, max(0, deadline - clock())))
    if pending:
        return result('unknown', 'approval_click_unconfirmed')
    if target_url and last_reason is None:
        return result('sent', 'approval_window_finished')
    return result('unknown', last_reason or 'approval_window_expired')
