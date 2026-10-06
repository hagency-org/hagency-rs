[English](README.md) | [中文](README.zh-CN.md)

# Hagency quick start and user guide

Run local Codex agents in Palpo Matrix projects and work with them in Rinx.
This guide follows the current server-engagement workflow: establish the
connection, create resources, request a project, then request an agent.

## Terms used in this guide

- **Hagency operator / resource owner**: runs Hagency on the Codex host and
  decides which resources and budgets to offer. The resource owner's Matrix
  account confirms server-connection requests.
- **Palpo administrator**: approves installing the server engagement on the
  Matrix server.
- **Coordinator**: an existing Matrix account delegated to approve project,
  agent and top-up requests for a server engagement. This is a human approval
  role; it does not require a coordinator AI agent.
- **Server engagement**: one authorized connection between Hagency and Palpo,
  with its own coordinator and resources. Hagency can keep multiple engagements
  separate. A **fleet** is the server-side registration and account namespace
  for that connection, not another token pool.
- **Resource**: a model, reasoning effort and token budget for one server
  engagement. Several resources may use the same local Codex account.
- **Source configuration**: an existing local configuration that supplies the
  coding framework, provider and account when creating a resource.
- **Project owner / agent owner**: the Matrix user who requests the project or
  agent. The agent owner receives its DM and execution-approval cards.
- **Agent engagement**: one agent's accepted allocation and runtime record,
  shown under Hagency's **Engagements**. It differs from a server engagement.
- **Project room**: the unencrypted room for project discussion and @mentions.
- **DM**: the encrypted private chat between an agent and its owner.
- **Approval room**: the separate encrypted room where the approval bot sends
  protected-operation requests to the owner.

## Who does what

| Role | Where | Responsibility |
| --- | --- | --- |
| Hagency operator / resource owner | Hagency console; Rinx Palpo Inbox | Configure local Codex, request and verify connections, create resources and manage delegation. |
| Palpo administrator | Rinx Palpo Inbox | Approve the server engagement. |
| Coordinator | Rinx Palpo Inbox | Review project, agent and additional-token requests within the delegated resources. |
| Project / agent owner | Rinx Palpo Resources, Projects and Agents; chat | Request a project and agent, accept the DM, send work and answer execution-approval cards. |
| Guest | Project room | @mention the agent; protected operations still need the agent owner's approval. |

One person can hold several roles. Rinx uses the currently signed-in Matrix
account; separate app profiles help when testing several roles on one computer.

## Before you start

You need:

- A Palpo server reachable over `https`, for example
  `https://matrix.your-server.example`.
- An administrator account on that Palpo server.
- A Hagency build containing this workflow and a compatible Rinx build with
  the bundled Palpo mini app. See [Get the binary](../../README.md#2-get-the-hagency-binary)
  for build instructions. Older pre-release binaries may expose the earlier
  manual JSON-import workflow instead.
- Existing resource-owner and coordinator Matrix accounts on the selected
  server. A coordinator is not created automatically by entering a name.
- Codex installed on the machine that runs Hagency. You sign it in yourself
  in Step 1.
- An owner account that has cross-signing set up in Rinx (for example, by
  setting up secure backup or verifying a session). Hagency waits until the
  owner has a cross-signing key before it creates an agent for them.

Steps 1 to 3 are local operator setup. Step 4 involves the resource owner and
administrator; Step 5 is resource configuration. Steps 6 to 8 involve the
project owner and coordinator.

## Step 1: Sign in to Codex

Agents use the Codex sign-in on the machine that runs Hagency. Keep that
machine and the Hagency service running while agents are working.

On that machine, sign Codex in yourself:

```bash
codex login
```

On a machine without a browser, add `--device-auth`.

Hagency never signs in for you, and it never reads or stores your
credentials. It only asks Codex whether it is signed in.

## Step 2: Start Hagency and open the console

1. Start Hagency in one of two ways:
   - **As a service (recommended):**

     ```bash
     hagency service install
     ```

     The service runs as you and starts again when you log in. On Linux it
     is a `systemd --user` unit, so you do not need `sudo`. To keep it
     running after you log out, run `loginctl enable-linger $USER` once.
   - **In the terminal:**

     ```bash
     hagency start
     ```

     It keeps running until you press Ctrl-C.

   Both keep Hagency's data in a default folder on this machine. The
   repository README describes both commands under
   [Start Hagency](../../README.md#3-start-hagency).
2. Hagency prints a console link and opens it in your browser. If no browser
   opens, open the printed link yourself in a browser on this machine.
3. The console opens and keeps you signed in until you click **End access**
   or close the browser. Restarting Hagency does not sign you out. The link
   keeps working until you print a new one, so keep it private.

To print a new link later:

```bash
# macOS
hagency console-access --state-dir "$HOME/Library/Application Support/Hagency"
# Linux
hagency console-access --state-dir "${XDG_DATA_HOME:-$HOME/.local/share}/hagency"
```

## Step 3: Set up the coding agent in the console

1. In the console menu, open **Setup**. The page has three steps:
   **Coding agents**, **Connect Palpo** and **Offer a resource**. A step
   shows a check mark when it is done. Until all three steps are done,
   every other console page shows a one-line note, "Setup is not
   finished", with a link to **Setup**.
2. Under **Coding agents**, Hagency shows Codex's path, its version and
   whether it is signed in.
   - If Codex is not installed, install it and click **Check again**.
   - If Codex is not signed in, run `codex login` in a terminal on this
     machine (Step 1) and click **Check again**.
3. When Codex is signed in, you do not need to click anything. When the page
   loads, Hagency configures itself to run Codex. The page then says
   "Hagency is configured to run this agent."

The page shows how Codex is signed in: **ChatGPT plan** or **API key**. A
ChatGPT plan sign-in is meant for personal use. Before you offer the agent to
other people, consider signing Codex in with an API key. The page notes this
but does not stop you.

## Step 4: Connect Palpo

1. **Operator:** in **Setup → Connect Palpo**, or on **Server engagements**,
   click **New server engagement**.
2. Fill in the HTTPS **Matrix server address**, resource owner's full Matrix
   ID, coordinator's full Matrix ID, connection name and delegation duration.
   For example, an account ID is `@owner:example.org`, not a display name and
   not an HTTPS URL. Both accounts must already exist on the selected server.
   Leave **Separate Palpo address** blank unless the administrator gives you a
   different operations address.
3. Click **Request connection**. Hagency saves the request and continues in the
   background; the page shows who needs to act next.
4. **Resource owner:** in Rinx, open **Mini apps → Palpo** (under **Discover** on mobile). Review
   the host consent and click **Run** if this is the first use. In **Inbox**,
   open the connection request and approve the one you initiated. The
   **Open request in Rinx** link in Hagency goes to the same request.
5. **Palpo administrator:** open that request in your own Rinx **Palpo → Inbox**
   and approve the server setup.
6. Hagency receives the approved configuration automatically. **No JSON file is
   needed for a new connection requested this way.**
7. **Resource owner:** open the approved request in your Rinx Inbox and click
   **Verify connection** once. It becomes disabled while verifying. Wait for
   **Connection verified** in Rinx and Hagency before creating resources.

Hagency creates the necessary service accounts; you do not create a separate
coordinator bot or connect a project-side server. Each server engagement has
its own credentials, delegation and resources.

To change the coordinator or expiry, use **Server engagements → Change
delegation**. **Cancel** closes the unsaved form. Suspending or revoking the
delegation stops new authorized allocations; it does not erase existing agents,
reservations or history. Revocation requires a new association if you later
want to reconnect. Do not revoke a working engagement just to add a resource.

## Step 5: Create a resource

The wizard needs an existing local **Source configuration**. If the list is
empty on a fresh installation, the operator must first create the local Codex
source with the [operator API](../../README.md#create-a-resource-with-the-operator-api).
**Setup** currently prepares the runtime but does not seed that first source;
opening Setup again does not create one. This is a current onboarding gap.

1. Open **My resources → New resource configuration**. The link in Setup opens
   the same wizard.
2. Select the verified **Server engagement**.
3. Under **Source configuration**, choose the local Codex configuration. The
   optional search filters framework, model or reasoning level. It does not
   ask for an agent, project or role. Paging controls appear only when more
   source configurations exist.
4. Choose a **Model** and **Reasoning** level from the available qualified
   choices.
5. On **Budget**, enter the monthly token budget and **Eligible Matrix project
   managers**. Use full user IDs on this server, separated by spaces or newlines,
   for example:

   ```text
   @project-owner:example.org
   @another-manager:example.org
   ```

   This list controls who may request a project on the resource; it is not a
   project-room member list. Include the coordinator's ID only if that account
   should also be eligible to request projects.
6. Click **Create resource**. Its creation and engagement allocation are saved
   together. Publication to Palpo is queued; wait until it appears in Rinx's
   **Palpo → Resources** under an eligible account.

One server engagement can have multiple resources. Repeat the same wizard to
add another model or budget. There is no separate allocation dialog or second
engagement token pool to fill afterward.

In **My resources**, use **Edit configuration** to change a resource or
**Withdraw from native catalog** to stop offering it. In-use resources may
refuse model changes; the budget cannot be reduced below consumed or reserved
tokens. Displayed budgets and shared-account declarations are allocation
facts, not measured usage or guaranteed provider capacity. **Unknown** account
quota does not mean zero.

## Step 6: Request a project and an agent

1. **Project owner:** in Rinx's **Palpo → Resources**, choose a resource and
   click **Request project here**. Enter the project name and purpose.
2. Leave the room choice empty to create a new project room, or click
   **Choose an existing room**. An eligible existing room is private
   (invite-only), unencrypted, not a Space, and created by your account.
   Submitting invites the engagement's representative to that room. The server
   checks the room again when processing the request.
3. Submit. **Coordinator:** approve or reject the project in **Palpo → Inbox**.
   **Owner:** wait for project setup to complete in **Projects**. Project
   approval grants access to the selected resource; it does not reserve an
   agent's token allocation.
4. **Owner:** click **Request agent** on the ready project. Choose the resource
   and role; enter the agent name, initial tokens and daily rate. Use the name
   rules shown by the form, then submit.
5. **Coordinator:** open the agent request in your Inbox, review the requested
   allocation and approve or reject it. Hagency validates the delivered decision and current capacity.
6. Follow the request under **Agents** or **Open latest result**. Approval and
   execution are separate:

   | Execution | Meaning |
   | --- | --- |
   | `pending` | Hagency has not finished accepting the decision. |
   | `provisioning` | The allocation was accepted; Matrix and runtime setup are in progress. |
   | `ready` | The runtime is ready for chat. |
   | `allocation_refused` | Hagency could not accept the allocation; inspect the stated reason. |
   | `provisioning_failed` / `provisioning_unknown` | Setup failed or its outcome is uncertain; the operator must inspect the original attempt. |
   | `unavailable` | The agent exists, but its runtime or Matrix connection needs attention. |

The project room is the shared place for discussion and agent @mentions. It is
not the agent's private workspace or approval room. Agent DMs and approval rooms
are encrypted separately. Agents do not work in encrypted group rooms shared
with other people.

## Step 7: Accept the DM and talk to the agent

1. **Owner:** accept the agent's DM invitation in Rinx when it appears. Watch
   the execution status until it is **ready**; seeing an account or invitation
   alone does not prove the runtime has started.
2. Send a short message such as “Reply with ready.” **No @mention is needed in
   the owner DM, including the first message.** Do not add other people to it.
3. In the project room, @mention the agent to give it work. Keep follow-ups in
   the same thread. A safe first test is a short reply that requires no file
   changes or external actions.
4. When testing protected operations, watch the separate owner approval room.

## Step 8: Approve agent actions

Some actions, such as running certain commands, need the owner's approval.

Project setup creates the private approval room and invites Hagency's approval
bot. Hagency verifies its owner and membership before accepting agent work.
Resource/project approvals in the Palpo Inbox and these runtime-operation
approvals are different decisions.

1. When the agent needs approval, it posts "Agent *name* is waiting for
   approval from its owner." in the project room.
2. Hagency's approval bot posts an approval card in the owner's approval
   room. The card names the agent, the project, the tool and the input, and
   shows when it expires.
3. Choose one button:
   - **Approve once**: allow this one action.
   - **Allow for this task**: allow this kind of action for the rest of the
     current task.
   - **Always allow this operation**: save this exact rule for this agent and
     project.
   - **Deny**: refuse the action.

   Some cards offer only **Approve once** and **Deny**. Typing a text reply
   does not count as an answer; use the buttons.

Hagency uses a separate approval-bot device for each owner. One owner's cards
are never encrypted for another owner.

## Work with an agent: owner and guest

Everyone who talks to an agent is either its **owner** or a **guest**. The
owner is the person who requested the agent. A guest is anyone else in a room
with the agent: a project member, or someone in another room the agent
joined.

| | Owner | Guest |
| --- | --- | --- |
| Where to talk to the agent | The DM, the project room, and any room the agent joined | The project room and unencrypted rooms the agent joined |
| How to get an answer | In the DM, or a room where the owner is the only person: any message. In a shared room: @mention the agent | @mention the agent; it answers in that message's thread |
| Follow-ups | Reply in the same thread | Reply in the same thread |
| Invite the agent to another room | The agent accepts on its own | The operator accepts or declines it in the console |
| Approve the agent's actions | Yes, with the cards in the approval room | No. Guests see "Agent *name* is waiting for approval from its owner." and wait |
| Tokens | The work of everyone, guests included, spends the owner's allocation | Spends the owner's allocation |
| Add tokens or end the agent's work | Uses authorized controls in Rinx **Palpo → Agents**; top-ups require coordinator approval | No |

### As the owner

1. Accept the agent's DM in Rinx and talk to it there. The DM is only for you
   and the agent; if anyone else joins it, the agent stops answering there.
2. In the project room, @mention the agent and keep the conversation in that
   message's thread.
3. To work with the agent in another room, invite it by its full Matrix ID
   (see [Use an agent in other rooms](#use-an-agent-in-other-rooms)). For a
   room with other people, turn encryption off when you create the room.
4. Watch your approval room. Every action that needs approval, whoever asked
   for it, comes to you as a card there. Until you answer, the agent waits.
5. When the agent pauses because its tokens ran out, request more in Rinx
   (see [Manage tokens](#manage-tokens)).

### As a guest

1. In the project room, or another unencrypted room the agent is in, @mention
   the agent. It answers in the thread of your message; keep follow-ups
   there.
2. If the agent posts that it is waiting for approval from its owner, the
   owner has to answer a card first. Ask the owner, not the agent.
3. You can invite the agent to a room of yours, but it joins only after the
   operator accepts, because its work there spends the owner's tokens.
4. In an encrypted room with other people, the agent does not work; it posts
   a notice instead. Use an unencrypted room.
5. You cannot DM the agent: it answers only its owner in a DM.

## Use an agent in other rooms

You can bring an agent into other rooms on the same server.

### Invite the agent

1. In Rinx, open the room and invite the agent by its full Matrix ID, for
   example `@hf_...:your-server.example`. You can see the agent's full ID in
   the member list of its DM.
2. You can also type the agent's name in Rinx's invite dialog. The search
   only finds people you share a room with and people the server's user
   directory returns, so the full ID is the most reliable way.

What happens next depends on who invited the agent:

- **The owner invited it:** the agent accepts on its own, usually within
  10 seconds.
- **Someone else invited it:** the invitation waits in the console under
  **Invitations**. The operator clicks **Accept** or **Decline**, because
  work in that room spends the owner's tokens.

### How the agent behaves in the room

| Room | What the agent does |
| --- | --- |
| Unencrypted room with other people | Answers messages that @mention it, in that message's thread. |
| Room where the only person is the owner (encrypted or not) | Answers every message from the owner. |
| Encrypted room with other people | Does not work there. It posts a notice that it cannot work in an encrypted room with other people in it. |

In an encrypted room shared with others, only the owner could read the
agent's replies, so the agent does not answer there. If others keep posting,
it repeats the notice at most once every 15 minutes. A Matrix room cannot
turn encryption off once it is on. To work with the agent and other people
together, create a new room with encryption off and invite the agent there.

The agent reads only messages sent after it joined. If you wrote before it
finished joining, send the message again.

Approvals and tokens work the same in every room. Approval cards always go to
the owner's approval room, never into the shared room.

## Manage tokens

A coordinator's approval accepts the requested allocation, subject to Hagency's
capacity checks. Runtime work consumes that agent allocation. If it runs out,
the agent pauses.

1. **Owner:** in Rinx's **Palpo → Agents**, click **Request more tokens** on the
   agent, enter the additional amount, and submit.
2. **Coordinator:** review the top-up in the Palpo Inbox.
3. Wait for the runtime result. A recorded approval is not proof that Hagency
   has accepted the additional allocation yet.

Authorized agent controls also appear here: **Rename agent**, **Pause agent**,
**Resume agent** and **Remove agent**. Use **Open latest result** to check the
runtime's response. Removal keeps chat history and completes only after
verified runtime and Matrix cleanup. Unsettled usage can retain a reservation.
Hagency's **Engagements** and **Usage** pages provide the operator's detailed
runtime and accounting view.

## Encryption and your devices

Agent DMs and approval rooms are end-to-end encrypted.

- Agent replies in the DM reach all of the owner's sessions, verified or
  not.
- Approval cards reach only sessions the owner has verified. A new Rinx
  session that you have not verified shows "Unable to decrypt" for approval
  cards. This is by design. Verify the session from another session, or
  with your recovery key, to see new cards.

## Troubleshooting

**The Inbox is empty.**
Check the signed-in Matrix account and the Inbox filter. Connection requests
first need the resource owner, then the administrator; project and agent requests
need the designated coordinator. Requests already decided may be under the
waiting or completed filter. Reading a notification does not approve a request.

**The agent is approved but not ready, or never sends a DM.**

- Read **Execution** and the explanation under **Open latest result**. A
  business approval does not by itself start Codex.
- Accept the owner DM invitation when it appears and ensure the owner has
  cross-signing set up in Rinx.
- Confirm the server engagement says **Connection verified** and local Codex
  is configured and signed in.
- For `provisioning_unknown`, retain the original request and ask the operator
  to inspect its effect, Matrix state and logs before retrying. An account or
  room may already exist and its allocation may still be reserved. Do not
  create duplicates or revoke the server engagement as a setup workaround.
- Check Hagency's “fleet service stage” log for `awaiting_runtime_config`
  (finish **Setup → Coding agents**) or `awaiting_reception` (finish connection
  verification). On macOS the service log is
  `~/Library/Logs/Hagency/hagency.log`; on Linux use
  `journalctl --user -u hagency`.

**The first DM needs an @mention.**
It should not. Confirm the execution state is ready and that this is the
agent's owner DM. If an ordinary message still gets no response, ask the
operator to inspect Matrix key exchange and runtime intake rather than treating
@mentions as a DM requirement.

**The log shows `refused_config`, or Setup says "The coding agent changed".**
Codex was updated, and Hagency's configuration still names the old Codex
binary.

1. Open **Setup** in the console. It updates the configuration by itself
   (click **Check again** if the note stays) and keeps the old file as a
   backup.
2. Restart Hagency, as the page tells you. Hagency reads its configuration
   only when it starts, so the running service keeps the old one until then:
   - macOS: `launchctl kickstart -k gui/$(id -u)/io.hagency`
   - Linux: `systemctl --user restart hagency`
   - In a terminal: stop `hagency start` with Ctrl-C and run it again.

Setup writes the configuration with the default Codex folder (`$CODEX_HOME`,
or `~/.codex`). If you set Hagency up with `hagency setup --codex-home` or
`--no-local-codex`, or you have no console, run
`hagency setup --state-dir <state> --force` with the same options instead,
then restart Hagency.

**The Approve button is greyed out.**
Check the request's current assignee, delegation expiry, resource availability
and stated refusal. Buttons depend on the signed-in account's authority.

**The agent ignores my messages in a group room.**
In a room with other people, the agent answers only when you @mention it.

**I cannot find the agent when I search for it.**
Matrix user search usually finds only people you already share a room with.
Invite the agent by its full Matrix ID.

**The agent says it cannot work in an encrypted room.**
Create a new room with encryption off and invite the agent there.

**Approval cards show "Unable to decrypt".**
Your current session is not verified. Verify it, or answer the card from a
verified session.

**The agent stopped and posted "Paused".**
Its token allocation is used up. Use **Palpo → Agents → Request more tokens**.

**Links in messages show no preview.**
Link previews are made by the homeserver, not by Hagency. Ask the Palpo
administrator to allow previews for the sites you need.

**Hagency is waiting for connection verification.**
Automatic configuration delivery follows owner confirmation and administrator
approval. In the resource owner's Rinx Inbox, open that same approved request
and click **Verify connection** once. Wait while its button says verification
is in progress. Downloading JSON is not part of this new-connection flow.

**There is no source configuration.**
This is the first-resource onboarding gap described in Step 5. Create the local
source through the documented operator API; then return to the resource wizard.

## Known limitations

- **Uncertain setup is not automatically safe to repeat.** The operator must
  inspect the original attempt before retrying an uncertain write. See
  [Troubleshooting](#troubleshooting).
- **A changed owner key cannot be accepted in the console.** Hagency trusts
  the cross-signing key it first sees for an owner. If the owner later resets
  cross-signing, the console has no control to trust the new key.
- **Joined rooms are not shown in the console.** The console does not list
  the rooms an agent joined after it was created.
- **No work in encrypted rooms shared with other people.** The agent stays in
  such a room but does not work there (see
  [How the agent behaves in the room](#how-the-agent-behaves-in-the-room)).
- **Each agent stays within its server engagement.** Multiple server
  engagements do not grant an agent access to another homeserver's users or rooms.
