window.__calls = [];
window.__replies = { connected: false, roster: [], secret_list: [], policy_list: [],
                     connectors: [], routines: [], groups: [], group_log: [], search: [],
                     skills: { skills: [], problems: [] } };
window.__listeners = {};
window.__TAURI__ = {
  core: {
    invoke: (cmd, args) => {
      window.__calls.push({ cmd, args });
      if (window.__throw && window.__throw[cmd]) {
        return Promise.reject(window.__throw[cmd]);
      }
      const reply = window.__replies[cmd];
      // A reply may be a function of the arguments, for commands whose answer
      // depends on them. Without that a search stub answers every query the
      // same way, and a test for "the wrong answer must not land" passes
      // whatever the page does.
      const value = typeof reply === "function" ? reply(args) : reply;
      return Promise.resolve(value === undefined ? null : value);
    },
  },
  event: {
    listen: (name, handler) => {
      (window.__listeners[name] = window.__listeners[name] || []).push(handler);
      return Promise.resolve(() => {});
    },
  },
};
window.__fire = (name, payload) => {
  for (const h of window.__listeners[name] || []) h({ payload });
};
window.__sent = (cmd) => window.__calls.filter((c) => c.cmd === cmd);
