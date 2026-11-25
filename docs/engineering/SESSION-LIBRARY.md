# Session library

Use **Sessions** to open a saved recording. The shared library lists finished and interrupted sessions by creation time. The recording manifest supplies duration and committed or missing chunk counts. Search matches the selected durable transcript text across saved sessions and returns up to 100 matches. Windows and Android use the same query and session IDs.

Editing a transcript inserts a manual revision with the original track, time and chunk references. Edits submitted against a changed revision are rejected, with the draft still visible. Previously saved translations remain available with their outdated-source label. Automated coverage exercises the same request flow with synthetic audio in Actions. [Issue #14](https://github.com/sagehou/VoiceLedger/issues/14)

Track playback, timeline detail, title and speaker search, and export remain part of #14. The existing Windows recording screen offers track playback. Physical interaction and accessibility checks are scheduled in #18. [Device acceptance](https://github.com/sagehou/VoiceLedger/issues/18)
