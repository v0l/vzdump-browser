// Rendering
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use crate::app::App;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ViewMode { FileList, VmaInfo, PartitionTable, HexView, FsBrowse }

pub fn render(frame: &mut Frame, app: &App) {
    let ch = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    let title = if !app.status_msg.is_empty() {
        format!("⏳ {}", app.status_msg)
    } else {
        match app.view_mode {
            ViewMode::FileList => format!("VZDump Browser — {}", app.current_path.display()),
            ViewMode::VmaInfo => {
                let n_devices = app.vma.as_ref()
                    .map(|a| a.devices.len())
                    .unwrap_or(0);
                format!("VMA: {} devices", n_devices)
            }
            ViewMode::PartitionTable => {
                let name = app.vma.as_ref()
                    .and_then(|v| v.devices.get(app.selected_device))
                    .map(|d| d.name.as_str())
                    .unwrap_or("?");
                format!("Partitions — {}", name)
            }
            ViewMode::FsBrowse => format!("Filesystem — {}", app.ext4_current_dir.1),
            ViewMode::HexView => format!("Hex — {}", app.hex_title),
        }
    };
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL)),
        ch[0],
    );

    match app.view_mode {
        ViewMode::FileList => render_filelist(frame, ch[1], app),
        ViewMode::VmaInfo => render_vma_info(frame, ch[1], app),
        ViewMode::PartitionTable => render_partitions(frame, ch[1], app),
        ViewMode::HexView => render_hex(frame, ch[1], app),
        ViewMode::FsBrowse => render_fsbrowse(frame, ch[1], app),
    }

    let status = match app.view_mode {
        ViewMode::FileList => {
            let s = app.list_state.selected().unwrap_or(0);
            if s < app.items.len() {
                let i = &app.items[s];
                format!("{} | {} | Enter=Open h=Back ?=Help",
                    app.current_path.display(),
                    if i.is_dir { "DIR".into() } else { crate::app::format_size(i.size) })
            } else {
                format!("{} | ?=Help", app.current_path.display())
            }
        }
        ViewMode::VmaInfo => "w/s=device  p/Enter=partitions  x=raw hex  t=back  ?=Help".into(),
        ViewMode::PartitionTable => {
            if let Some(ref dp) = app.dump_path {
                format!("j/k=select  Enter=hex  x=raw  d=dump:{}  h=back", dp.display())
            } else {
                "j/k=select  Enter=hex  x=raw hex  h=back  ?=Help".into()
            }
        }
        ViewMode::FsBrowse => {
            format!("{} type={} blocks={} | Enter=open e=extract ?=Help",
                app.ext4_current_dir.1,
                app.ext4_fs_type,
                app.ext4_total_blocks)
        }
        ViewMode::HexView => "j/k=scroll  PgUp/Dn=page  h=back  ?=Help".into(),
    };
    frame.render_widget(
        Paragraph::new(status)
            .style(Style::default().fg(Color::White).bg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL)),
        ch[2],
    );

    if app.show_help { render_help(frame); }
}

fn render_filelist(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app.items.iter().map(|i| {
        let icon = if i.is_dir { "📁" } else if i.is_vma { "📦" } else { "📄" };
        let n = i.path.file_name().unwrap_or_default().to_string_lossy();
        let sz = if i.is_dir { String::new() } else { format!("  {}", crate::app::format_size(i.size)) };
        ListItem::new(format!("{} {:<40}{}", icon, n, sz))
    }).collect();
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Backups "))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("> "),
        area,
        &mut app.list_state.clone(),
    );
}

fn render_vma_info(frame: &mut Frame, area: Rect, app: &App) {
    use std::fmt::Write;
    let mut c = String::new();
    if let Some(v) = &app.vma {
        writeln!(c, "Created: {}", v.ctime).ok();
        writeln!(c, "Devices: {}  Configs: {}", v.devices.len(), v.config.len()).ok();
        writeln!(c, "\n── Configs (j/k scroll) ──").ok();
        let s = app.config_scroll.min(v.config.len().saturating_sub(1));
        let e = (s + 15).min(v.config.len());
        for i in s..e {
            let (n, vv) = &v.config[i];
            let p: String = vv.chars().take(70).filter(|c| *c != '\n').collect();
            writeln!(c, "  {:<35} {}", n, p).ok();
        }
        writeln!(c, "\n── Devices (w/s switch) ──").ok();
        for (i, d) in v.devices.iter().enumerate() {
            let sel = if i == app.selected_device { "▶" } else { " " };
            let tag = if app.has_device_data() { " [decompressed]" } else { "" };
            writeln!(c, "{} {:<25} {:>10}{}",
                sel, d.name, crate::app::format_size(d.size), tag).ok();
        }
    }
    writeln!(c, "\np/Enter: Analyze partitions").ok();
    writeln!(c, "x: Raw hex  w/s: Switch  t: Back").ok();
    frame.render_widget(
        Paragraph::new(c).style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title(" VMA Info ")),
        area,
    );
}

fn render_partitions(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app.partitions.iter().map(|p| {
        let sz = crate::app::format_size(p.num_sectors * 512);
        ListItem::new(format!("{}  {:>10}  {:>10}  {:>6}  {}", p.name, p.start_sector, sz, p.type_guid, p.fs_type))
    }).collect();
    let mut s = ListState::default();
    s.select(Some(app.partition_scroll));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Partitions "))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("> "),
        area,
        &mut s,
    );
}

fn render_hex(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(
        Paragraph::new(gen_hex(&app.hex_data, app.hex_scroll))
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title(" Hex View ")),
        area,
    );
}

fn render_fsbrowse(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app.ext4_dirlist.iter().map(|e| {
        let icon = match e.file_type { 2 => "📁", 1 => "📄", 7 => "🔗", _ => "❓" };
        let name = if e.name.len() > 60 {
            format!("{}...", &e.name[..57])
        } else {
            e.name.clone()
        };
        ListItem::new(format!("{} {}", icon, name))
    }).collect();
    let mut s = app.ext4_dirlist_state.clone();
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL)
                .title(format!(" {} ({})", app.ext4_current_dir.1, app.ext4_dirlist.len())))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("> "),
        area,
        &mut s,
    );
}

fn render_help(frame: &mut Frame) {
    let h = vec![
        ListItem::new("Help"), ListItem::new("────"),
        ListItem::new("q/t/h   Quit / Back"),
        ListItem::new("j/k     Navigate / Scroll"),
        ListItem::new("Enter   Open / Select"),
        ListItem::new("e       Extract file (fs browser)"),
        ListItem::new("x       Raw hex view"),
        ListItem::new("w/s     Switch device"),
        ListItem::new("PgUp/Dn Page scroll"),
        ListItem::new(""),
        ListItem::new("Header: instant. Extraction: lazy, bounded."),
        ListItem::new("Detects: MBR, ext4, XFS, btrfs, NTFS, LVM2"),
    ];
    let a = centered_rect(55, 55, frame.area());
    frame.render_widget(Clear, a);
    frame.render_widget(
        List::new(h)
            .block(Block::default().borders(Borders::ALL).title(" Help ")
                .border_style(Style::default().fg(Color::Yellow)))
            .style(Style::default().fg(Color::White)),
        a,
    );
}

pub fn gen_hex(data: &[u8], scroll: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if data.is_empty() { return "No data".into(); }
    let start = scroll * 16;
    let end = (start + 24 * 16).min(data.len());
    for i in (start..end).step_by(16) {
        let chunk = &data[i..(i + 16).min(data.len())];
        write!(s, "{:08X}  ", i).unwrap();
        for b in chunk.iter().take(8) { write!(s, "{:02X} ", b).unwrap(); }
        s.push(' ');
        for b in chunk.iter().skip(8).take(8) { write!(s, "{:02X} ", b).unwrap(); }
        s.push_str(" |");
        for b in chunk { s.push(if *b >= 32 && *b <= 126 { *b as char } else { '.' }); }
        s.push_str("|\n");
    }
    s
}

fn centered_rect(px: u16, py: u16, r: Rect) -> Rect {
    let pop = Layout::default().direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - py) / 2),
            Constraint::Percentage(py),
            Constraint::Percentage((100 - py) / 2),
        ]).split(r);
    Layout::default().direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - px) / 2),
            Constraint::Percentage(px),
            Constraint::Percentage((100 - px) / 2),
        ]).split(pop[1])[1]
}
