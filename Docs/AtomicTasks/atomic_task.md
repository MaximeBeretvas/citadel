An **atomic task** is the smallest piece of work that still makes sense on its own. "Atomic" means you can't usefully split it further: it's either fully done or not done.

In the LLM workflow of your note, a task counts as atomic when it is:

1. **Small enough for one agent session.** The agent can do it in one context window without losing track, for example one endpoint, one UI component or one pipeline step.
2. **Self-contained.** It only builds on work that is already finished and tested, so the agent doesn't have to guess what comes before it.
3. **Checkable.** It ends with one concrete check: a test passes, the types check, or the component renders.
4. **One thing only.** It doesn't mix steps like "build the data model *and* the UI".

**Example from your note:**

- ❌ Not atomic: *"User can open and sort files"* is a whole user story. The agent would have to invent the data model, the logic, the API and the UI all at once.
- ✅ Atomic: *"Write the sort function plus its unit tests."* It's one step and you know when it's done (the tests pass).

**Quick test:** if you can't say in one sentence how you'll know the task is done, it isn't atomic yet. Split it.