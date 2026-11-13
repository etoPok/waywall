use std::collections::HashMap;

use anyhow::{Context, Ok};

use crate::wayland::ipc::{Compositor, CompositorEvent, OutputId, ToplevelId, WorkspaceId};

#[derive(Clone, Debug)]
pub struct Toplevel {
    id: ToplevelId,
    fullscreen: bool,
    maximized: bool,
    workspace: WorkspaceId,
}

impl Toplevel {
    pub fn new(id: ToplevelId, fullscreen: bool, workspace: WorkspaceId) -> Self {
        Self {
            id,
            fullscreen,
            maximized: false,
            workspace,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Workspace {
    id: WorkspaceId,

    // Mirroring/Clone
    // Two or more outputs may share the same workspace (not necessarily the active workspace).
    output: OutputId,
}

impl Workspace {
    pub fn new(id: WorkspaceId, output: OutputId) -> Self {
        Self { id, output }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Output {
    name: OutputId,
    lock: bool,
    active_workspace: WorkspaceId,
}

#[derive(Clone, Debug, Default)]
pub struct OcclusionTracker<C: Compositor> {
    compositor: C,
    active_output: String,
    active_workspace: WorkspaceId,
    active_toplevel: Option<ToplevelId>,

    toplevels: HashMap<ToplevelId, Toplevel>,
    workspaces: HashMap<WorkspaceId, Workspace>,
    outputs: HashMap<OutputId, Output>,
}

impl<C: Compositor> OcclusionTracker<C> {
    pub fn new(
        compositor: C,
        active_output: OutputId,
        active_workspace: WorkspaceId,
        active_toplevel: Option<ToplevelId>,
        toplevels: &[(String, bool, i64, i64)],
        workspaces: &[(i64, String)],
        outputs: &[(i64, String, i64, bool)],
    ) -> anyhow::Result<Self> {
        Ok(Self {
            compositor,
            active_output: active_output.clone(),
            active_workspace,
            active_toplevel,
            toplevels: toplevels
                .iter()
                .map(|t| (t.0.clone(), Toplevel::new(t.0.clone(), t.1, t.2)))
                .collect(),
            workspaces: workspaces
                .iter()
                .map(|w| (w.0, Workspace::new(w.0, w.1.clone())))
                .collect(),
            outputs: outputs
                .iter()
                .map(|o| {
                    if !workspaces.iter().any(|w| w.0 == o.2) {
                        anyhow::bail!(
                            "OcclusionTracker.new: the provided workspaces do not contain the output's active workspace"
                        )
                    }
                    Ok((
                        o.1.clone(),
                        Output {
                            name: o.1.clone(),
                            active_workspace: o.2,
                            lock: toplevels.iter().any(|t| t.2 == o.2 && t.1),
                        },
                    ))
                })
                .collect::<anyhow::Result<HashMap<_, _>>>()?,
        })
    }

    /// Add a toplevel to the active workspace.
    fn open_toplevel(&mut self, id: &ToplevelId) -> anyhow::Result<()> {
        if self.toplevels.contains_key(id) {
            anyhow::bail!(
                "OcclusionTracker.open_toplevel: the toplevel already exists in the workspace"
            )
        } else {
            self.toplevels.insert(
                id.clone(),
                Toplevel::new(id.clone(), false, self.active_workspace),
            );
            Ok(())
        }
    }

    pub fn close_toplevel(&mut self, id: &ToplevelId) -> anyhow::Result<()> {
        let toplevel = self
            .toplevels
            .get(id)
            .context("OcclusionTracker.close_toplevel: the toplevel does not exist")?;
        if !self.workspaces.contains_key(&toplevel.workspace) {
            anyhow::bail!(
                "OcclusionTracker.close_toplevel: the workspace of the toplevel does not exist"
            )
        }
        if toplevel.workspace
            != self
                .outputs
                .get(&self.active_output)
                .context("OcclusionTracker.close_toplevel: the active output does not exist")?
                .active_workspace
        {
            anyhow::bail!(
                "OcclusionTracker.close_toplevel: the toplevel to close is not in the active workspace"
            )
        }
        self.toplevels.remove(id);
        let active_output = self
            .outputs
            .get_mut(&self.active_output)
            .context("OcclusionTracker.close_toplevel: the active output does not exist")?;
        active_output.lock = self
            .toplevels
            .values()
            .any(|t| t.workspace == active_output.active_workspace && t.fullscreen);
        Ok(())
    }

    /// Set the fullscreen state of the active toplevel and update the active output's lock
    /// status.
    ///
    /// Only the active toplevel can be updated. After applying `fullscreen`, the active
    /// output's `lock` is recomputed: it is locked if any toplevel is fullscreen and on that
    /// output's active workspace.
    ///
    /// This function will fail if:
    /// - there is no active toplevel
    /// - the active toplevel does not exist
    /// - the workspace of the active toplevel does not exist
    /// - the active output does not exist
    /// - the active toplevel is not in the active workspace of the active output
    fn fullscreen(&mut self, fullscreen: bool) -> anyhow::Result<()> {
        let active_toplevel = if let Some(t) = &self.active_toplevel {
            if let Some(toplevel) = self.toplevels.get_mut(t) {
                toplevel
            } else {
                anyhow::bail!("OcclusionTracker.fullscreen: the active toplevel does not exist")
            }
        } else {
            anyhow::bail!("OcclusionTracker.fullscreen: there is no active toplevel")
        };
        if !self.workspaces.contains_key(&active_toplevel.workspace) {
            anyhow::bail!(
                "OcclusionTracker.fullscreen: the workspace of the active toplevel does not exist"
            )
        }
        let active_output = self
            .outputs
            .get_mut(&self.active_output)
            .context("OcclusionTracker.fullscreen: the active output does not exist")?;
        if active_output.active_workspace != active_toplevel.workspace {
            anyhow::bail!(
                "OcclusionTracker.fullscreen: the active toplevel is not in the active workspace of the active output"
            )
        }
        active_toplevel.fullscreen = fullscreen;
        active_output.lock = self
            .toplevels
            .values()
            .any(|t| t.workspace == active_output.active_workspace && t.fullscreen);
        Ok(())
    }

    /// Move a toplevel to a workspace.
    ///
    /// This function will fail if:
    /// - the toplevel to move does not exist
    /// - the current workspace of the toplevel to move does not exist
    /// - the target workspace does not exist
    /// - the toplevel to move already exists in the target workspace
    /// - the output of the current workspace does not exist
    /// - the output of the target workspace does not exist
    ///
    /// The output does not change when moving between workspaces on the same output.
    fn move_toplevel(
        &mut self,
        toplevel_id: &ToplevelId,
        target_workspace_id: &WorkspaceId,
    ) -> anyhow::Result<()> {
        let toplevel_to_move = self
            .toplevels
            .get(toplevel_id)
            .context("OcclusionTracker.move_toplevel: the toplevel to move does not exist")?;
        let target_workspace = self
            .workspaces
            .get(target_workspace_id)
            .context("OcclusionTracker.move_toplevel: the target workspace does not exist")?;
        let current_workspace = self.workspaces.get(&toplevel_to_move.workspace).context(
            "OcclusionTracker.move_toplevel: the current workspace of the toplevel to move does not exist",
        )?;
        // Check using the already validated workspaces
        if current_workspace.id == target_workspace.id {
            anyhow::bail!(
                "OcclusionTracker.move_toplevel: the toplevel to move already exists in the target workspace"
            )
        }
        self.toplevels.get_mut(toplevel_id).unwrap().workspace = *target_workspace_id;
        if current_workspace.output != target_workspace.output {
            {
                let current_output = self.outputs.get_mut(&current_workspace.output).context(
                    "OcclusionTracker.move_toplevel: the output of the current workspace does not exist",
                )?;
                if current_output.active_workspace == current_workspace.id {
                    current_output.lock = self
                        .toplevels
                        .values()
                        .any(|t| current_output.active_workspace == t.workspace && t.fullscreen);
                }
            }
            let target_output = self.outputs.get_mut(&target_workspace.output).context(
                "OcclusionTracker.move_toplevel: the output of the target workspace does not exist",
            )?;
            if target_output.active_workspace == target_workspace.id {
                target_output.lock = self
                    .toplevels
                    .values()
                    .any(|t| target_output.active_workspace == t.workspace && t.fullscreen);
            }
        } else {
            let output = self.outputs.get_mut(&target_workspace.output).context(
                "OcclusionTracker.move_toplevel: the output shared by the current and target workspaces does not exist",
            )?;
            // Check that both workspaces belong to the same output.
            if output.active_workspace == current_workspace.id
                || output.active_workspace == target_workspace.id
            {
                output.lock = self
                    .toplevels
                    .values()
                    .any(|t| output.active_workspace == t.workspace && t.fullscreen);
            }
        }
        Ok(())
    }

    /// Update the active toplevel
    fn active_toplevel(&mut self, id: &ToplevelId) -> anyhow::Result<()> {
        self.active_toplevel = if id.trim().is_empty() {
            None
        } else {
            if !self.toplevels.contains_key(id) {
                anyhow::bail!(
                    "OcclusionTracker.active_toplevel: the toplevel to activate does not exist"
                )
            }
            Some(id.clone())
        };
        Ok(())
    }

    /// Activate a workspace on the active output.
    ///
    /// This function will fail if:
    /// - the workspace to activate does not exist
    /// - the active output does not exist
    /// - the workspace to activate does not exist on the active output
    ///
    /// The `OutputFocused` and `WorkspaceCreated` or `WorkspaceMoved` events must be
    /// handled before calling this function. The `WorkspaceCreated` and `ActiveWorkspace`
    /// events may occur with a workspace that does not belong to the active output, and the
    /// `OutputFocused` or `WorkspaceMoved` events are expected to determine which output
    /// the workspace belongs to.
    fn active_workspace(&mut self, workspace_id: &WorkspaceId) -> anyhow::Result<()> {
        let workspace_to_active = self.workspaces.get(workspace_id).context(
            "OcclusionTracker.active_workspace: the workspace to activate does not exist",
        )?;
        let active_output = self
            .outputs
            .get_mut(&self.active_output)
            .context("OcclusionTracker.active_workspace: the active output does not exist")?;
        if active_output.name != workspace_to_active.output {
            anyhow::bail!(
                "OcclusionTracker.active_workspace: the workspace to activate does not exist on the active output"
            )
        }
        self.active_workspace = workspace_to_active.id;
        active_output.active_workspace = workspace_to_active.id;
        active_output.lock = self
            .toplevels
            .values()
            .any(|t| active_output.active_workspace == t.workspace && t.fullscreen);
        Ok(())
    }

    /// Add an output in an unlocked state, using the active output's active workspace.
    /// This function will fail if the active output does not exist.
    fn add_output(&mut self, output: &OutputId) -> anyhow::Result<()> {
        if !self.outputs.contains_key(output) {
            let active_workspace = self
                .outputs
                .get(&self.active_output)
                .context("OcclusionTracker.add_output: the active output does not exist")?
                .active_workspace;
            self.outputs.insert(
                output.clone(),
                Output {
                    name: output.clone(),
                    lock: false,
                    active_workspace,
                },
            );
        }
        Ok(())
    }

    /// Remove an output.
    ///
    /// This function will fail if:
    /// - the active output does not exist
    /// - the output to remove does not exist
    /// - the output to remove is the same as the active output
    /// - the workspaces of the output to remove have not been destroyed or moved
    ///
    /// The active output must always be known, and the `OutputFocused` event must be
    /// handled before calling this function to set a valid output as active. Likewise,
    /// the `WorkspaceDestroyed` or `WorkspaceMoved` events must be handled for each
    /// workspace of the output to remove before calling this function.
    fn remove_output(&mut self, output_id: &OutputId) -> anyhow::Result<()> {
        let output_to_remove = self
            .outputs
            .get(output_id)
            .context("OcclusionTracker.remove_output: the output to remove does not exist")?;
        if self
            .outputs
            .get(&self.active_output)
            .context("OcclusionTracker.remove_output: the active output does not exist")?
            .name
            == output_to_remove.name
        {
            anyhow::bail!(
                "OcclusionTracker.remove_output: the output to remove is the same as the active output"
            )
        }
        if self
            .workspaces
            .values()
            .any(|w| w.output == output_to_remove.name)
        {
            anyhow::bail!(
                "OcclusionTracker.remove_output: the workspaces of the output to remove have not been destroyed or moved"
            )
        }
        self.outputs.remove(output_id);
        Ok(())
    }

    /// Make an output active and move the workspace to it.
    /// This function will fail if the workspace or the output to activate does not exist.
    fn focus_output(
        &mut self,
        output_id: &OutputId,
        workspace_id: &WorkspaceId,
    ) -> anyhow::Result<()> {
        let workspace_to_move = self
            .workspaces
            .get_mut(workspace_id)
            .context("OcclusionTracker.focus_output: the workspace to activate does not exist")?;
        self.outputs
            .get_mut(output_id)
            .context(
                "OcclusionTracker.focus_output: the output to activate or focus does not exist",
            )?
            .active_workspace = *workspace_id;
        workspace_to_move.output = output_id.clone();
        self.active_output = output_id.clone();
        Ok(())
    }

    /// Create a workspace directly on the active output.
    ///
    /// This function will fail if:
    /// - the active output does not exist
    /// - the new workspace already exists
    ///
    /// The `WorkspaceCreated` event may occur with a workspace that does not belong to the
    /// active output, and the `OutputFocused` or `WorkspaceMoved` events are expected to
    /// determine which output the workspace belongs to.
    fn create_workspace(&mut self, workspace_id: &WorkspaceId) -> anyhow::Result<()> {
        if !self.outputs.contains_key(&self.active_output) {
            anyhow::bail!("OcclusionTracker.create_workspace: the active output does not exist")
        }
        if self.workspaces.contains_key(workspace_id) {
            anyhow::bail!(
                "OcclusionTracker.create_workspace: the workspace to create already exists"
            )
        }
        self.workspaces.insert(
            *workspace_id,
            Workspace::new(*workspace_id, self.active_output.clone()),
        );
        Ok(())
    }

    /// Destroy a workspace.
    /// This function will fail if the workspace to remove does not exist.
    fn destroy_workspace(&mut self, workspace_id: &WorkspaceId) -> anyhow::Result<()> {
        if self.workspaces.remove(workspace_id).is_none() {
            anyhow::bail!(
                "OcclusionTracker.destroy_workspace: the workspace to destroy does not exist"
            )
        }
        if self.active_workspace == *workspace_id {
            // Compositor may destroy the active workspace without sending the `ActiveWorkspace` event.
            let active_workspace = self.compositor.active_workspace()?;
            let active_workspace = self.workspaces.get(&active_workspace).context(
                "OcclusionTracker.destroy_workspace: the active workspace does no exist",
            )?;
            self.outputs
                .get_mut(&active_workspace.output)
                .context("OcclusionTracker.destroy_workspace: the active output does not exist")?
                .active_workspace = active_workspace.id;
            self.active_workspace = active_workspace.id;
        }
        Ok(())
    }

    /// Move a workspace to an output. Adds the target output if it does not exist.
    ///
    /// This function will fail if:
    /// - the workspace to move does not exist
    /// - the active output does not exist when the target output needs to be added
    ///
    /// If the workspace is already on the target output, the function returns early
    /// without error.
    fn move_workspace(
        &mut self,
        workspace_id: &WorkspaceId,
        target_output_id: &OutputId,
    ) -> anyhow::Result<()> {
        if !self.outputs.contains_key(target_output_id) {
            self.add_output(target_output_id)?;
        }
        let workspace_to_move = self
            .workspaces
            .get_mut(workspace_id)
            .context("OcclusionTracker.move_workspace: the workspace to move does not exist")?;

        // This can happen if an output is added and a workspace is created for it, but the
        // output is removed before focusing it and determining via the `OutputFocused` event
        // whether that workspace really belongs to it.
        if workspace_to_move.output == *target_output_id {
            // anyhow::bail!(
            //     "OcclusionTracker.move_workspace: the workspace to move already exists on the target output"
            // )
            return Ok(());
        }

        workspace_to_move.output = target_output_id.clone();
        Ok(())
    }

    /// Get the lock status of the outputs
    pub fn get_output_status(&self) -> HashMap<OutputId, bool> {
        self.outputs
            .iter()
            .map(|(id, output)| (id.clone(), output.lock))
            .collect()
    }

    pub fn handle_event(&mut self, event: &CompositorEvent) -> anyhow::Result<()> {
        if let CompositorEvent::OutputAdded(output_id) = event {
            tracing::info!("Dispatch: OutputAdded {output_id}");
            return self.add_output(output_id);
        }

        if let CompositorEvent::OutputRemoved(output_id) = event {
            tracing::info!("Dispatch: OutputRemoved {output_id}");
            return self.remove_output(output_id);
        }

        if let CompositorEvent::OutputFocused(output_id, workspace_id) = event {
            tracing::info!("Dispatch: OutputFocused {output_id} {workspace_id}");
            return self.focus_output(output_id, workspace_id);
        }

        if let CompositorEvent::ActiveToplevel(toplevel_id) = event {
            tracing::info!("Dispatch: ActiveToplevel {toplevel_id}");
            return self.active_toplevel(toplevel_id);
        }

        if let CompositorEvent::OpenToplevel(toplevel_id) = event {
            tracing::info!("Dispatch: OpenToplevel {toplevel_id}");
            return self.open_toplevel(toplevel_id);
        }

        if let CompositorEvent::CloseToplevel(toplevel_id) = event {
            tracing::info!("Dispatch: CloseToplevel {toplevel_id}");
            return self.close_toplevel(toplevel_id);
        }

        if let CompositorEvent::ToplevelMoved(toplevel_id, workspace_id) = event {
            tracing::info!("Dispatch: ToplevelMoved {toplevel_id} {workspace_id}");
            return self.move_toplevel(toplevel_id, workspace_id);
        }

        if let CompositorEvent::Fullscreen(fullscreen) = event {
            tracing::info!("Dispatch: Fullscreen {fullscreen}");
            return self.fullscreen(*fullscreen);
        }

        if let CompositorEvent::ActiveWorkspace(workspace_id) = event {
            tracing::info!("Dispatch: ActiveWorkspace {workspace_id}");
            return self.active_workspace(workspace_id);
        }

        if let CompositorEvent::WorkspaceMoved(workspace_id, output_id) = event {
            tracing::info!("Dispatch: WorkspaceMoved {workspace_id} {output_id}");
            return self.move_workspace(workspace_id, output_id);
        }

        if let CompositorEvent::WorkspaceCreated(workspace_id) = event {
            tracing::info!("Dispatch: WorkspaceCreated {workspace_id}");
            return self.create_workspace(workspace_id);
        }

        if let CompositorEvent::WorkspaceDestroyed(workspace_id) = event {
            tracing::info!("Dispatch: WorkspaceDestroyed {workspace_id}");
            return self.destroy_workspace(workspace_id);
        }

        Ok(())
    }
}
