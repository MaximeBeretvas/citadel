# web-vault

A local vault that ingests documentation websites and Markdown folders into a searchable store that agents and people can query.

## Language

### Sources

**Source**:
A registered origin of pages: either a web URL or a local Markdown folder.
_Avoid_: site, repo, collection

**Origin**:
The URL or canonical folder path that identifies a Source; unique across the vault.
_Avoid_: root, base URL, seed

**Private origin**:
An Origin that points at loopback, private-network, link-local or local hostnames; refused unless the vault allows private origins.
_Avoid_: internal URL, SSRF target

**Source lifecycle**:
Everything that changes a Source: adding, updating, removing and syncing it.
_Avoid_: source service, source manager

### Syncing

**Sync**:
One pass that brings a Source's pages in line with its Origin.
_Avoid_: refresh, crawl, ingest (crawl and walk are how a Sync fetches, not the Sync itself)

**Sync run**:
The recorded history entry of one Sync, including its counts and any error.
_Avoid_: job, task

**Sync claim**:
The vault-wide exclusive right to Sync one Source; at most one holder at a time, across every process.
_Avoid_: lock, lease, in-flight

### Pages

**Page**:
One document from a Source, identified by its URI.

**Stale page**:
A Page that was present before but missing from a complete Sync; kept, not deleted. A Sync cut short by its page limit leaves unvisited Pages as they were.

**Error page**:
A Page whose latest fetch or parse failed.
