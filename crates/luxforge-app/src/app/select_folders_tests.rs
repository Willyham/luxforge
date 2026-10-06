//! Indexed folders and sending photographs back in Select, against a real owner over a scratch
//! catalog and copied fixtures: Add a folder… sends the `index.add-folder` an agent writes, as an
//! owner task, its listing followed to its end and the folder then listed in the sources panel;
//! `Cmd+Z` undoes it; a folder dropped on the window is added the same way, and a file dropped is
//! refused with the core's reason; Remove from indexed folders… asks first and sends
//! `index.remove-folder`. Send back, from the Info panel and from a right-click's menu, sends
//! `asset.send-back` of the selection as one library change, the photograph leaving the catalog and
//! its file picked again, and a refusal says the core's reason. Each owner call runs exactly as its
//! task would, and each request is compared with what an independent client writes and its effect
//! read back through that client.
use crate::{
    Config,
    app::{
        Boot, Editor,
        message::{
            Message, select::SelectMessage, select_catalog::CatalogMessage, sync::SyncMessage,
        },
        select::{add_folder_now, job_now, label_now, session_now},
        select_owner_tests::{answer_reads, evaluate, read_rows},
        tasks::{call, owner_calls},
    },
    state::{
        select::{Shown, SourcePress},
        select_catalog::{CatalogAction, CatalogMenu},
    },
};
use iced::{Point, Size};
use luxforge_core::{ClientId, EditorService, OwnerHandle, catalog_types::ViewSource};
use luxforge_testbase::{
    paths::{fixture, temp_dir},
    wait_for,
};
use luxforge_ui::{GridContext, GridPress, PressModifiers};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The editor over a new scratch catalog with Select shown, an independent client of the same
/// owner, and the scratch directory.
fn scene(name: &str) -> (Editor, ClientId, PathBuf) {
    let dir = temp_dir(&format!("select-folders-{name}"))
        .canonicalize()
        .unwrap();
    let catalog = dir.join("catalog.sqlite");
    drop(EditorService::open(&catalog).unwrap());
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let (mut editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let agent = editor.owner.register();
    settle(&mut editor);
    (editor, agent, dir)
}

fn finish(mut editor: Editor, dir: PathBuf) {
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    let _ = fs::remove_dir_all(dir);
}

/// `count` fixture JPEGs copied into `folder`.
fn copies(folder: &Path, count: u32) -> Vec<PathBuf> {
    fs::create_dir_all(folder).unwrap();
    (1..=count)
        .map(|at| {
            let path = folder.join(format!("DSC_000{at}.jpg"));
            fs::copy(fixture(&format!("s0/orientation-{at}.jpg")), &path).unwrap();
            path
        })
        .collect()
}

/// One request through the independent client.
fn ask(editor: &Editor, agent: ClientId, method: &str, params: Value) -> Value {
    call(&editor.owner, agent, method, params)
        .unwrap_or_else(|error| panic!("{method}: {error}"))
        .0
}

/// Run what the editor has asked the owner for, as its tasks would, until nothing is in flight: the
/// reads Select makes, a staleness check, an evaluation, a change's label, the rows near the screen,
/// the catalog's lists, an `index.add-folder` in flight and its listing's record once it has ended.
fn settle(editor: &mut Editor) {
    for _ in 0..12 {
        answer_reads(editor);
        if editor.select.check.in_flight() {
            let checked = session_now(&editor.owner, editor.client).map(Box::new);
            let _ = editor.update(Message::Select(SelectMessage::Checked(checked)));
        }
        if editor.select.state.loading {
            evaluate(editor);
        }
        if let Some(sequence) = editor.select.label {
            let result = label_now(&editor.owner, editor.client, sequence);
            let _ = editor.update(Message::Select(SelectMessage::Labelled {
                sequence,
                result,
            }));
        }
        read_rows(editor);
        if editor.select.catalog.lists.in_flight() {
            let lists = crate::app::select_catalog::lists_now(&editor.owner, editor.client);
            let _ = editor.update(Message::Select(SelectMessage::Catalog(
                CatalogMessage::Lists(lists),
            )));
        }
        if let Some(adding) = editor.select.adding.clone() {
            match adding.job {
                // The request in flight, run as its task runs it.
                None => {
                    let params = adding.request.clone();
                    let result = add_folder_now(&editor.owner, editor.client, params.clone());
                    let _ = editor.update(Message::Select(SelectMessage::FolderAdded {
                        params,
                        result,
                    }));
                }
                // The authoritative reader waits for the listing result, independently of the
                // activity board and without a polling interval.
                Some(job) => {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .unwrap();
                    let record = runtime.block_on(async {
                        tokio::time::timeout(luxforge_testbase::HANG, async {
                            let mut after = None;
                            loop {
                                let (change, record) = super::job_reads::wait(
                                    &editor.owner,
                                    editor.client,
                                    &job,
                                    after,
                                )
                                .await
                                .unwrap();
                                after = Some(change);
                                if !matches!(record["status"].as_str(), Some("queued" | "running"))
                                {
                                    break record;
                                }
                            }
                        })
                        .await
                        .expect("the folder's listing to end")
                    });
                    let _ = editor.update(Message::Select(SelectMessage::AddListed {
                        job,
                        result: Ok(record),
                    }));
                }
            }
        }
        if editor.select_reads_quiet() {
            return;
        }
    }
    panic!("Select did not settle: {}", editor.select_summary());
}

/// The indexed folders the sources panel lists, as `(name, count, state)`.
fn listed(editor: &Editor) -> Vec<(String, String, Option<String>)> {
    editor
        .workspace
        .select
        .sources
        .indexed
        .iter()
        .map(|row| {
            let count = match &row.count {
                crate::state::select::Count::Total(total) => total.clone(),
                _ => String::new(),
            };
            (row.name.clone(), count, row.secondary.clone())
        })
        .collect()
}

/// The journal's newest change, read through the independent client.
fn newest(editor: &Editor, agent: ClientId) -> Value {
    ask(editor, agent, "library.journal", json!({"limit": 500}))["changes"]
        .as_array()
        .and_then(|changes| changes.last())
        .cloned()
        .expect("a change")
}

/// Add a folder… sends `index.add-folder` of the chosen folder as an owner task, the request an
/// agent writes; its listing is followed to its end, and the sources panel then lists the folder
/// with what the listing found, the status bar saying the change with the key that takes it back.
/// `Cmd+Z` undoes it. A folder dropped on the window is added the same way, and a file dropped is
/// refused with the core's reason. Remove from indexed folders…, from the row's menu, asks first;
/// its Remove sends `index.remove-folder` synchronously, which `Cmd+Z` undoes.
#[test]
fn select_adds_lists_undoes_and_removes_an_indexed_folder_on_a_real_owner() {
    use iced::keyboard::Modifiers;
    let (mut editor, agent, dir) = scene("add");
    let folder = dir.join("Card dumps");
    let files = copies(&folder, 3);
    assert!(listed(&editor).is_empty(), "no indexed folder yet");

    // The dialog's folder: an owner task, never sent from the update loop.
    owner_calls::take();
    let _ = editor.update(Message::Select(SelectMessage::AddFolderPicked(Some(
        folder.clone(),
    ))));
    assert!(owner_calls::take().is_empty(), "sent from its own task");
    let request = editor.select.adding.as_ref().unwrap().request.clone();
    let request_id = request["mutation"]["request_id"].clone();
    assert_eq!(
        request,
        json!({"path": folder, "mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    assert!(!editor.select_reads_quiet(), "the add is in flight");
    assert!(
        editor.status.text.starts_with("Adding "),
        "{}",
        editor.status.text
    );
    // Cmd+Z waits for it rather than undoing something older.
    let _ = editor.update(Message::Select(SelectMessage::Undo));
    assert_eq!(editor.status.text, "Waiting for the folder being added");
    settle(&mut editor);
    let library = editor.select.library.clone().unwrap();
    assert_eq!(library["method"], "index.add-folder");
    assert!(library["answer"]["job_id"].is_string(), "{library}");
    assert_eq!(
        listed(&editor),
        vec![("Card dumps".to_owned(), "3".to_owned(), None)]
    );
    let change = newest(&editor, agent);
    assert_eq!(
        (
            &change["actor"],
            &change["method"],
            &change["label"],
            &change["request_id"]
        ),
        (
            &json!("desktop"),
            &json!("index.add-folder"),
            &json!("Added Card dumps to indexed folders"),
            &request_id
        )
    );
    assert_eq!(
        editor.status.text,
        "Added Card dumps to indexed folders \u{b7} Undo \u{2318}Z"
    );
    let folders = ask(&editor, agent, "index.folders", json!({}));
    assert_eq!(folders["folders"][0]["path"], json!(folder));
    assert_eq!(folders["folders"][0]["files"], 3);
    // Pressing its row views it with its subfolders.
    let row = editor.workspace.select.sources.indexed[0].clone();
    assert_eq!(
        row.press,
        Some(SourcePress::View(ViewSource::Folder {
            path: folder.clone(),
            subfolders: true
        }))
    );
    let Some(SourcePress::View(source)) = row.press else {
        unreachable!()
    };
    let _ = editor.update(Message::Select(SelectMessage::Source(source)));
    settle(&mut editor);
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 3);
    assert!(editor.workspace.select.sources.indexed[0].selected);

    // Cmd+Z undoes it: one synchronous `library.undo`, and the folder leaves the list.
    owner_calls::take();
    let key = |editor: &mut Editor, letter: &str, modifiers: Modifiers| {
        use iced::keyboard::{
            Event as KeyEvent, Key, Location,
            key::{NativeCode, Physical},
        };
        let pressed = Key::Character(letter.into());
        let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: pressed.clone(),
            modified_key: pressed,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        });
        let _ = editor.update(Message::Key(event, iced::event::Status::Ignored));
    };
    key(&mut editor, "z", Modifiers::COMMAND);
    assert_eq!(owner_calls::take(), vec!["library.undo".to_owned()]);
    settle(&mut editor);
    assert!(listed(&editor).is_empty(), "{:?}", listed(&editor));
    assert_eq!(
        editor.status.text,
        "Undid Added Card dumps to indexed folders \u{b7} Redo \u{21e7}\u{2318}Z"
    );
    assert!(
        ask(&editor, agent, "index.folders", json!({}))["folders"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Dropped on the window while Select is shown: added the same way.
    let _ = editor.update(Message::Select(SelectMessage::Dropped(folder.clone())));
    assert_eq!(
        editor.select.adding.as_ref().unwrap().request["path"],
        json!(folder)
    );
    settle(&mut editor);
    assert_eq!(listed(&editor)[0].0, "Card dumps");
    // A file dropped is refused with the core's reason; nothing is added.
    let _ = editor.update(Message::Select(SelectMessage::Dropped(files[0].clone())));
    settle(&mut editor);
    assert!(
        editor.status.text.starts_with("Could not add the folder: "),
        "{}",
        editor.status.text
    );
    assert_eq!(listed(&editor).len(), 1);

    // Remove from indexed folders…: the row's menu, its confirmation, then one synchronous change.
    let context = editor.workspace.select.sources.indexed[0].context.clone();
    assert_eq!(context, Some(SourcePress::Menu(Some(folder.clone()))));
    let _ = editor.update(Message::Select(SelectMessage::IndexedMenu(Some(
        folder.clone(),
    ))));
    let menu = editor.workspace.select.sources.indexed[0]
        .menu
        .clone()
        .unwrap();
    assert_eq!(menu[0].press, Some(SourcePress::Forget(folder.clone())));
    let _ = editor.update(Message::Select(SelectMessage::AskForget(Some(
        folder.clone(),
    ))));
    let sheet = editor.workspace.select.forget.clone().unwrap();
    assert_eq!(sheet.title, "Remove Card dumps from indexed folders?");
    // Escape (the shell's menu message) puts it away and sends nothing.
    owner_calls::take();
    let _ = editor.update(Message::Select(SelectMessage::Menu(None)));
    assert!(editor.workspace.select.forget.is_none());
    assert!(owner_calls::take().is_empty());
    let _ = editor.update(Message::Select(SelectMessage::AskForget(Some(
        folder.clone(),
    ))));
    let _ = editor.update(Message::Select(SelectMessage::Forget));
    assert_eq!(owner_calls::take(), vec!["index.remove-folder".to_owned()]);
    let library = editor.select.library.clone().unwrap();
    let request_id = library["params"]["mutation"]["request_id"].clone();
    assert_eq!(
        library["params"],
        json!({"path": folder, "mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    settle(&mut editor);
    assert!(listed(&editor).is_empty());
    assert_eq!(
        newest(&editor, agent)["label"],
        "Removed Card dumps from indexed folders"
    );
    assert_eq!(
        editor.status.text,
        "Removed Card dumps from indexed folders \u{b7} Undo \u{2318}Z"
    );
    key(&mut editor, "z", Modifiers::COMMAND);
    settle(&mut editor);
    assert_eq!(listed(&editor)[0].0, "Card dumps");

    // In Develop a drop never adds a folder.
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    let _ = editor.update(Message::Select(SelectMessage::Dropped(folder.clone())));
    assert!(editor.select.adding.is_none());
    for file in &files {
        assert!(file.exists(), "a file is never touched");
    }
    finish(editor, dir);
}

/// Three fixtures developed through the independent client, as an agent develops them, and the
/// editor viewing All photographs with every row read. Answers their file names.
fn developed(editor: &mut Editor, agent: ClientId, dir: &Path) -> Vec<PathBuf> {
    let folder = dir.join("Lake");
    let files = copies(&folder, 3);
    let started = ask(
        editor,
        agent,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": folder}}),
    );
    let job = started["job_id"].as_str().unwrap().to_owned();
    wait_for("the folder's listing", || {
        let record = job_now(&editor.owner, agent, &job).unwrap();
        (record["status"] == "ready").then_some(())
    });
    let targets = json!({"kind": "paths", "paths": files});
    let mutation = |id: &str| json!({"request_id": format!("agent-{id}"), "actor": "agent"});
    ask(
        editor,
        agent,
        "pick.set",
        json!({"targets": targets, "picked": true, "mutation": mutation("pick")}),
    );
    let started = ask(
        editor,
        agent,
        "pick.develop",
        json!({"into": [], "targets": targets, "mutation": mutation("develop")}),
    );
    let job = started["job_id"].as_str().unwrap().to_owned();
    wait_for("the Develop", || {
        let record = job_now(&editor.owner, agent, &job).unwrap();
        (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(())
    });
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    )));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    settle(editor);
    files
}

fn press(editor: &mut Editor, item: u32) {
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(item).unwrap());
    let _ = editor.update(Message::Select(SelectMessage::Press(GridPress {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        modifiers: PressModifiers::default(),
        double: false,
    })));
    settle(editor);
}

fn act(editor: &mut Editor, action: CatalogAction) {
    let _ = editor.update(Message::Select(SelectMessage::Catalog(
        CatalogMessage::Act(action),
    )));
}

/// Send back, from the Info panel, sends `asset.send-back` of the selection as one synchronous
/// library change, as an agent writes it: the photograph leaves the catalog and its file is picked
/// again by this desktop, and the status bar says so. A right-click on another photograph selects
/// it and opens its menu; its Send back, refused by the core because the photograph is in a
/// collection, says the core's reason and changes nothing. `Cmd+Z` after a send-back is refused by
/// the core, which records no undo of one.
#[test]
fn select_sends_a_developed_photograph_back_on_a_real_owner() {
    let (mut editor, agent, dir) = scene("send-back");
    developed(&mut editor, agent, &dir);
    let summary = editor.select.state.summary.clone().unwrap();
    assert_eq!(summary.count, 3);
    let name =
        |editor: &Editor, at: u32| editor.select.state.rows.row(at).unwrap().file_name.clone();

    // One photograph: the Info panel offers Send back, not refused.
    press(&mut editor, 0);
    let first = name(&editor, 0);
    let info = editor.workspace.select.catalog.info.clone().unwrap();
    let button = info.send_back.clone().unwrap();
    assert_eq!((button.label.as_str(), button.refused), ("Send back", None));
    owner_calls::take();
    act(&mut editor, CatalogAction::SendBack);
    assert_eq!(owner_calls::take(), vec!["asset.send-back".to_owned()]);
    let library = editor.select.library.clone().unwrap();
    let request_id = library["params"]["mutation"]["request_id"].clone();
    assert_eq!(
        library["params"],
        json!({"targets": {"kind": "selection"}, "mutation": {"request_id": request_id, "actor": "desktop"}})
    );
    assert!(library["error"].is_null(), "{library}");
    settle(&mut editor);
    assert_eq!(
        editor.status.text,
        format!("Sent back {first} \u{b7} its file is picked again")
    );
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 2);
    let change = newest(&editor, agent);
    assert_eq!(
        (&change["actor"], &change["method"], &change["label"]),
        (
            &json!("desktop"),
            &json!("asset.send-back"),
            &json!(format!("Sent back {first}"))
        )
    );
    let info = ask(&editor, agent, "catalog.info", json!({}));
    assert_eq!(info["counts"]["photographs"], 2);
    let picks = ask(&editor, agent, "pick.list", json!({}));
    let pick = picks["picks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pick| {
            pick["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(&first))
        })
        .cloned()
        .expect("the file picked again");
    assert_eq!(pick["actor"], "desktop");

    // An agent puts the next photograph in a collection; the wake reads it.
    let second = name(&editor, 1);
    let asset = match &editor.select.state.rows.row(1).unwrap().item {
        luxforge_core::catalog_types::RowItem::Photo { asset_id } => asset_id.clone(),
        other => panic!("a photograph: {other:?}"),
    };
    let created = ask(
        &editor,
        agent,
        "collection.create",
        json!({"name": "Keep", "kind": "collection", "mutation": {"request_id": "agent-keep", "actor": "agent"}}),
    );
    ask(
        &editor,
        agent,
        "collection.add",
        json!({"collection_id": created["collection"]["id"], "targets": {"kind": "assets", "asset_ids": [asset]}, "mutation": {"request_id": "agent-add", "actor": "agent"}}),
    );
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    settle(&mut editor);

    // A right-click on it selects it alone and opens the photographs' menu where it was.
    let layout = &editor.select.layout;
    let cell = layout.cell(layout.cell_of_item(1).unwrap());
    let rect = layout.item_rect(1).unwrap();
    let at = Point::new(rect.center_x(), rect.center_y());
    let _ = editor.update(Message::Select(SelectMessage::Context(GridContext {
        cell: cell.cell,
        item: cell.item,
        span: cell.span,
        at,
    })));
    settle(&mut editor);
    assert_eq!(editor.session.browse.selection.active, Some(1));
    assert_eq!(editor.select.state.catalog.menu, Some(CatalogMenu::Photos));
    let menu = editor.workspace.select.catalog.context.clone().unwrap();
    assert_eq!((menu.x, menu.y), (at.x, at.y));
    assert_eq!(menu.choices[0].label, "Send back");
    let send = menu.choices[0]
        .action
        .clone()
        .expect("the rows say nothing against it");
    let before = newest(&editor, agent)["sequence"].clone();
    act(&mut editor, send);
    assert!(
        editor.workspace.select.catalog.context.is_none(),
        "the menu closed"
    );
    assert_eq!(
        editor.status.text,
        format!(
            "Could not send back: {second} is in a collection: it leaves the catalog only by \
             being removed"
        )
    );
    settle(&mut editor);
    assert_eq!(
        newest(&editor, agent)["sequence"],
        before,
        "nothing changed"
    );
    assert_eq!(editor.select.state.summary.as_ref().unwrap().count, 2);

    // Cmd+Z: the core records no undo of a send-back, and says so.
    let _ = editor.update(Message::Select(SelectMessage::Undo));
    assert!(
        editor.status.text.starts_with("Could not undo: ")
            && editor.status.text.contains("was sent back to its picks"),
        "{}",
        editor.status.text
    );
    finish(editor, dir);
}
