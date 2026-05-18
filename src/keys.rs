// Key handlers
use anyhow::Result;
use crossterm::event::{self, KeyCode};
use crate::app::App;
use crate::ui::ViewMode;

pub fn handle_filelist(k: event::KeyEvent, app: &mut App) -> Result<()> {
    match k.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(s) = app.list_state.selected() {
                if s > 0 { app.list_state.select(Some(s - 1)); }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(s) = app.list_state.selected() {
                if s < app.items.len().saturating_sub(1) { app.list_state.select(Some(s + 1)); }
            }
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            if let Some(s) = app.list_state.selected() {
                if s < app.items.len() {
                    let i = app.items[s].clone();
                    if i.is_dir { let _ = app.load_directory(&i.path); }
                    else if i.is_vma { app.pending_op = Some(crate::app::PendingOp::LoadVma(i.path)); }
                }
            }
        }
        KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
            let parent = app.current_path.parent().map(|p| p.to_path_buf());
            if let Some(p) = parent { let _ = app.load_directory(&p); }
        }
        _ => {}
    }
    Ok(())
}

pub fn handle_vma(k: event::KeyEvent, app: &mut App) -> Result<()> {
    match k.code {
        KeyCode::Char('q') | KeyCode::Char('t') => app.view_mode = ViewMode::FileList,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.config_scroll > 0 { app.config_scroll -= 1; }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let mx = app.vma.as_ref().map(|v| v.config.len()).unwrap_or(0);
            if app.config_scroll < mx.saturating_sub(1) { app.config_scroll += 1; }
        }
        KeyCode::Char('w') => {
            if app.selected_device > 0 {
                app.selected_device -= 1;
                app.clear_device();
            }
        }
        KeyCode::Char('s') => {
            let mx = app.vma.as_ref().map(|v| v.devices.len()).unwrap_or(0);
            if app.selected_device + 1 < mx {
                app.selected_device += 1;
                app.clear_device();
            }
        }
        KeyCode::Enter | KeyCode::Char('p') => { app.pending_op = Some(crate::app::PendingOp::LoadPartitions); }
        KeyCode::Char('x') => { app.pending_op = Some(crate::app::PendingOp::LoadDeviceRaw); }
        _ => {}
    }
    Ok(())
}

pub fn handle_partitions(k: event::KeyEvent, app: &mut App) -> Result<()> {
    match k.code {
        KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Char('t') => {
            app.view_mode = ViewMode::VmaInfo;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.partition_scroll > 0 { app.partition_scroll -= 1; }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.partition_scroll < app.partitions.len().saturating_sub(1) { app.partition_scroll += 1; }
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            if !app.partitions.is_empty() {
                let p = &app.partitions[app.partition_scroll];
                if p.fs_type.contains("ext") || p.fs_type.contains("ext4/xfs") {
                    app.pending_op = Some(crate::app::PendingOp::BrowsePartitionFs(app.partition_scroll));
                } else {
                    app.pending_op = Some(crate::app::PendingOp::LoadPartitionHex(app.partition_scroll));
                }
            }
        }
        KeyCode::Char('x') => { app.pending_op = Some(crate::app::PendingOp::LoadDeviceRaw); }
        _ => {}
    }
    Ok(())
}

pub fn handle_hex(k: event::KeyEvent, app: &mut App) -> Result<()> {
    match k.code {
        KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Char('t') => {
            if !app.partitions.is_empty() { app.view_mode = ViewMode::PartitionTable; }
            else if app.vma.is_some() { app.view_mode = ViewMode::VmaInfo; }
            else { app.view_mode = ViewMode::FileList; }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.hex_scroll > 0 { app.hex_scroll -= 1; }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let mx = if app.hex_data.is_empty() { 0 } else { (app.hex_data.len() / 16).saturating_sub(1) };
            if app.hex_scroll < mx { app.hex_scroll += 1; }
        }
        KeyCode::PageUp => app.hex_scroll = app.hex_scroll.saturating_sub(20),
        KeyCode::PageDown => app.hex_scroll += 20,
        _ => {}
    }
    Ok(())
}

pub fn handle_fsbrowse(k: event::KeyEvent, app: &mut App) -> Result<()> {
    match k.code {
        KeyCode::Char('q') | KeyCode::Char('t') => {
            app.view_mode = ViewMode::PartitionTable;
        }
        KeyCode::Backspace | KeyCode::Char('h') => {
            let _ = app.fs_navigate_back();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let s = app.ext4_dirlist_state.selected().unwrap_or(0);
            if s > 0 { app.ext4_dirlist_state.select(Some(s - 1)); }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let s = app.ext4_dirlist_state.selected().unwrap_or(0);
            if s < app.ext4_dirlist.len().saturating_sub(1) {
                app.ext4_dirlist_state.select(Some(s + 1));
            }
        }
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            if let Some(s) = app.ext4_dirlist_state.selected() {
                let entry = app.ext4_dirlist[s].clone();
                if entry.file_type == 2 {
                    // Directory - navigate into it
                    let _ = app.fs_navigate_into(s);
                } else if entry.file_type == 1 {
                    // Regular file - show hex view
                    let _ = app.fs_show_file_hex(s);
                }
            }
        }
        KeyCode::Char('e') => {
            // Extract file to current working directory
            if let Some(s) = app.ext4_dirlist_state.selected() {
                let entry = app.ext4_dirlist[s].clone();
                if entry.file_type == 1 {
                    let dest = std::path::PathBuf::from(&entry.name);
                    let _ = app.fs_extract_file(s, &dest);
                }
            }
        }
        _ => {}
    }
    Ok(())
}
