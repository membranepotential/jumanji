# Plugins

A plugin is a small program Lighthouse runs at fixed points of a build. It
receives the content model as JSON and may return changes to it.

The points are listed in [Plugin hooks](../reference/plugin-hooks.md); where
they sit in a build is shown in [[pipeline]].
