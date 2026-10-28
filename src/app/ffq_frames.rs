use banqi_core::core::env::{
    DarkChessEnv, Piece, PieceType, Player, PositionSnapshot, Slot, SnapshotEnv,
    action_lookup_tables,
};
use serde_json::Value;

fn client_to_sq(x: i64, y: i64) -> Option<usize> {
    if !(0..4).contains(&x) || !(0..8).contains(&y) {
        return None;
    }
    Some(((3 - x) * 8 + y) as usize)
}

fn piece_from_code(code: i64) -> Option<Piece> {
    if !(0..14).contains(&code) {
        return None;
    }
    let piece_type = match code % 7 {
        0 => PieceType::Soldier,
        1 => PieceType::Cannon,
        2 => PieceType::Horse,
        3 => PieceType::Chariot,
        4 => PieceType::Elephant,
        5 => PieceType::Advisor,
        _ => PieceType::General,
    };
    let player = if code < 7 { Player::Red } else { Player::Black };
    Some(Piece::new(piece_type, player))
}

fn f_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

fn coord(v: &Value, norm_key: &str, raw_keys: &[&str]) -> Option<(i64, i64)> {
    if let Some(p) = v.get(norm_key) {
        if let (Some(x), Some(y)) = (f_i64(p, "x"), f_i64(p, "y")) {
            return Some((x, y));
        }
    }
    for pair in raw_keys.chunks(2) {
        if let (Some(x), Some(y)) = (f_i64(v, pair[0]), f_i64(v, pair[1])) {
            return Some((x, y));
        }
    }
    None
}

enum Event {
    Open {
        sq: usize,
        piece: Piece,
    },

    Move {
        from: usize,
        to: usize,
        reveal: Option<Piece>,
    },
}

fn tile_to_code(tile: i64) -> Option<i64> {
    let base = match tile {
        0..=4 | 16..=20 => 0,
        5 | 6 | 21 | 22 => 1,
        7 | 8 | 23 | 24 => 3,
        9 | 10 | 25 | 26 => 2,
        11 | 12 | 27 | 28 => 4,
        13 | 14 | 29 | 30 => 5,
        15 | 31 => 6,
        _ => return None,
    };
    Some(if tile < 16 { base } else { base + 7 })
}

fn parse_frame(v: &Value) -> Result<Option<Event>, String> {
    if v.get("k").and_then(Value::as_str) != Some("mv") {
        return Ok(None);
    }
    let cmd = f_i64(v, "cmd").ok_or("mv 帧缺少 cmd")?;

    let frame_cells: Option<Vec<i64>> = v.get("cells").and_then(|c| c.as_array()).map(|a| {
        a.iter()
            .map(|x| x.as_i64().unwrap_or(-1))
            .collect::<Vec<_>>()
    });
    let cell_piece_at = |x: i64, y: i64| -> Option<Piece> {
        let i = (y * 4 + x) as usize;
        frame_cells
            .as_ref()
            .and_then(|c| c.get(i))
            .copied()
            .filter(|c| (0..14).contains(c))
            .and_then(piece_from_code)
    };

    let piece_of = |tile: Option<i64>, x: i64, y: i64| -> Option<Piece> {
        tile.and_then(tile_to_code)
            .and_then(piece_from_code)
            .or_else(|| cell_piece_at(x, y))
    };
    match cmd {
        1 => {
            let Some((x, y)) = coord(v, "from", &["x", "y"]) else {
                if f_i64(v, "seq").is_none() {
                    return Ok(None);
                }
                return Err("翻子帧缺少坐标".into());
            };
            let sq = client_to_sq(x, y).ok_or_else(|| format!("翻子坐标越界 ({x},{y})"))?;
            let tile = f_i64(v, "open").or_else(|| f_i64(v, "iChessType"));
            let piece = piece_of(tile, x, y)
                .ok_or_else(|| format!("翻子帧无法确定棋子（tile={:?}）", tile.unwrap_or(-1)))?;
            Ok(Some(Event::Open { sq, piece }))
        }
        4 => {
            let Some((fx, fy)) = coord(v, "from", &["iFromX", "iFromY"]) else {
                if f_i64(v, "seq").is_none() {
                    return Ok(None);
                }
                return Err("走子帧缺少起点坐标".into());
            };
            let Some((tx, ty)) = coord(v, "to", &["iToX", "iToY"]) else {
                if f_i64(v, "seq").is_none() {
                    return Ok(None);
                }
                return Err("走子帧缺少终点坐标".into());
            };
            let from = client_to_sq(fx, fy).ok_or_else(|| format!("起点越界 ({fx},{fy})"))?;
            let to = client_to_sq(tx, ty).ok_or_else(|| format!("终点越界 ({tx},{ty})"))?;

            let tile = f_i64(v, "open")
                .or_else(|| f_i64(v, "iMoveChessOpen"))
                .filter(|&t| t >= 0)
                .or_else(|| {
                    f_i64(v, "eat")
                        .or_else(|| f_i64(v, "iBeEatChess"))
                        .filter(|&t| t >= 0)
                });
            let reveal = piece_of(tile, tx, ty);
            Ok(Some(Event::Move { from, to, reveal }))
        }
        _ => Err(format!("未知 mv cmd {cmd}")),
    }
}

fn initial_env_from_frames(
    target: &PositionSnapshot,
    frames: &[Value],
) -> Result<DarkChessEnv, String> {
    let cfg = target.variant.config();

    let first_mv = frames
        .iter()
        .position(|v| v.get("k").and_then(Value::as_str) == Some("mv"));
    let cells = frames
        .iter()
        .take(first_mv.unwrap_or(frames.len()))
        .filter_map(|v| {
            v.get("cells")
                .filter(|c| c.as_array().map(|a| a.len()).unwrap_or(0) == cfg.total_positions)
        })
        .last()
        .ok_or("帧窗缺少 cells 锚点（begin/ctx），无法构造初始局面")?;
    let arr = cells.as_array().expect("已校验为数组");

    let mut slots = vec![Slot::Empty; cfg.total_positions];
    for (i, v) in arr.iter().enumerate() {
        let code = v.as_i64().ok_or_else(|| format!("cells 第 {i} 格非整数"))?;
        let (x, y) = ((i % 4) as i64, (i / 4) as i64);
        let sq = client_to_sq(x, y).ok_or("cells 坐标越界")?;
        slots[sq] = match code {
            c if (0..14).contains(&c) => {
                Slot::Revealed(piece_from_code(c).expect("0-13 必为合法棋子"))
            }
            c if c >= 30 => Slot::Hidden,
            _ => Slot::Empty,
        };
    }

    let mut bag: Vec<Piece> = Vec::with_capacity(cfg.total_positions);
    for player in [Player::Red, Player::Black] {
        for t in 0..cfg.num_active {
            let piece_type = PieceType::from_index(cfg.active_types[t]);
            let mut n = cfg.piece_counts[cfg.active_types[t]];
            for s in &slots {
                if matches!(s, Slot::Revealed(p) if p.player == player && p.piece_type == piece_type)
                {
                    n -= 1;
                }
            }
            for _ in 0..n {
                bag.push(Piece::new(piece_type, player));
            }
        }
    }
    let snap = PositionSnapshot {
        variant: target.variant,
        slots,
        current_player: Player::Red,
        move_counter: 0,
        total_step_counter: 0,
        scores: [cfg.initial_health; 2],
        last_action: -1,
        hidden_bag: bag,
        dead_counts: [[0; banqi_core::core::env::NUM_PIECE_TYPES_MAX]; 2],
        true_board: None,
        chase_moves: vec![],
    };
    DarkChessEnv::from_snapshot(&snap).map_err(|e| format!("初始局面构造失败: {e}"))
}

pub fn replay_frames(
    target: &PositionSnapshot,
    frames: &[Value],
) -> Result<PositionSnapshot, String> {
    let mut env = initial_env_from_frames(target, frames)?;
    let lookup = action_lookup_tables(&env.config);
    let mut applied = 0usize;
    for (i, v) in frames.iter().enumerate() {
        let event = match parse_frame(v).map_err(|e| format!("第 {i} 帧: {e}"))? {
            Some(ev) => ev,
            None => continue,
        };
        let action = match &event {
            Event::Open { sq, .. } => lookup.single_action(*sq),
            Event::Move { from, to, .. } => lookup.pair_action(*from, *to),
        }
        .ok_or_else(|| format!("第 {i} 帧: 坐标不构成合法动作"))?;
        let reveal = match &event {
            Event::Open { piece, .. } => Some(*piece),
            Event::Move { reveal, .. } => *reveal,
        };

        let reveal = if env.is_chance_action(action) {
            reveal
        } else {
            None
        };
        if reveal.is_none() && env.is_chance_action(action) {
            return Err(format!("第 {i} 帧: 翻开/炮击暗子但缺少合法 tile id"));
        }

        env.step_unchecked(action, reveal)
            .map_err(|e| format!("第 {i} 帧: 动作 {action} 回放失败: {e}"))?;
        applied += 1;
    }
    if applied == 0 {
        return Err("帧窗内无有效 mv 事件".into());
    }
    let mut post = env.to_snapshot();
    if post.slots != target.slots {
        let diff = post
            .slots
            .iter()
            .zip(target.slots.iter())
            .filter(|(a, b)| a != b)
            .count();

        let cfg = target.variant.config();
        let fmt = |s: &Slot| match s {
            Slot::Empty => "空".into(),
            Slot::Hidden => "暗".into(),
            Slot::Revealed(p) => {
                let code = if p.player == Player::Red {
                    p.piece_type as usize
                } else {
                    p.piece_type as usize + 7
                };
                format!("子{code}")
            }
        };
        let detail = post
            .slots
            .iter()
            .zip(target.slots.iter())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(6)
            .map(|(sq, (a, b))| {
                let r = sq / cfg.cols;
                let c = sq % cfg.cols;
                format!(
                    "格{}(x{},y{}):回放{}≠快照{}",
                    c * 4 + (3 - r),
                    3 - r,
                    c,
                    fmt(a),
                    fmt(b)
                )
            })
            .collect::<Vec<_>>()
            .join("，");
        return Err(format!(
            "回放终局面与快照不一致（{diff} 格差异，回放 {} 手）：{detail}",
            post.total_step_counter
        ));
    }
    post.current_player = target.current_player;
    Ok(post)
}

#[cfg(test)]
mod tests {
    use super::*;
    use banqi_core::core::env::{SnapshotEnv, Variant};
    use serde_json::json;

    fn sq_to_client(sq: usize) -> (i64, i64) {
        ((3 - (sq / 8)) as i64, (sq % 8) as i64)
    }

    fn all_hidden_snapshot(variant: Variant) -> PositionSnapshot {
        let cfg = variant.config();
        let mut bag = Vec::with_capacity(cfg.total_positions);
        for player in [Player::Red, Player::Black] {
            for t in 0..cfg.num_active {
                let pt = PieceType::from_index(cfg.active_types[t]);
                for _ in 0..cfg.piece_counts[cfg.active_types[t]] {
                    bag.push(Piece::new(pt, player));
                }
            }
        }
        PositionSnapshot {
            variant,
            slots: vec![Slot::Hidden; cfg.total_positions],
            current_player: Player::Red,
            move_counter: 0,
            total_step_counter: 0,
            scores: [cfg.initial_health; 2],
            last_action: -1,
            hidden_bag: bag,
            dead_counts: [[0; banqi_core::core::env::NUM_PIECE_TYPES_MAX]; 2],
            true_board: None,
            chase_moves: vec![],
        }
    }

    #[test]
    fn replay_roundtrip_matches_snapshot() {
        let initial = all_hidden_snapshot(Variant::DarkChess4x8);
        let mut env = DarkChessEnv::from_snapshot(&initial).expect("初始快照构造");
        let mut rng: u64 = 0x2026_1028;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut frames: Vec<Value> = vec![json!({
            "k": "begin", "cmd": 63,
            "cells": vec![30; initial.slots.len()],
        })];
        for step in 0..40 {
            let legal = env.legal_action_indices();
            if legal.is_empty() {
                break;
            }
            let action = legal[(next() % legal.len() as u64) as usize];
            let cfg = env.config;
            let lookup = action_lookup_tables(&cfg);
            let (from, to) = if action < cfg.reveal_actions_count {
                let sq = lookup.action_to_coords[action][0];
                (sq, sq)
            } else {
                (
                    lookup.action_to_coords[action][0],
                    lookup.action_to_coords[action][1],
                )
            };
            let is_chance = env.is_chance_action(action);

            let mut chosen: Option<Piece> = None;
            if is_chance {
                chosen = env.to_snapshot().hidden_bag.first().copied();
            }
            let code = chosen.map(|p| {
                let base = match p.piece_type {
                    PieceType::Soldier => 0,
                    PieceType::Cannon => 1,
                    PieceType::Horse => 2,
                    PieceType::Chariot => 3,
                    PieceType::Elephant => 4,
                    PieceType::Advisor => 5,
                    PieceType::General => 6,
                };
                if p.player == Player::Red {
                    base
                } else {
                    base + 7
                }
            });
            let res = env.step(action, chosen);
            if res.is_err() {
                break;
            }

            let mut cells = vec![-1i64; cfg.total_positions];
            for (sq, slot) in env.to_snapshot().slots.iter().enumerate() {
                let (x, y) = sq_to_client(sq);
                cells[(y * 4 + x) as usize] = match slot {
                    Slot::Hidden => 30,
                    Slot::Empty => -1,
                    Slot::Revealed(p) => {
                        let base = match p.piece_type {
                            PieceType::Soldier => 0,
                            PieceType::Cannon => 1,
                            PieceType::Horse => 2,
                            PieceType::Chariot => 3,
                            PieceType::Elephant => 4,
                            PieceType::Advisor => 5,
                            PieceType::General => 6,
                        };
                        if p.player == Player::Red {
                            base
                        } else {
                            base + 7
                        }
                    }
                };
            }
            let (fx, fy) = sq_to_client(from);

            let tile_id = code
                .map(|c| {
                    if c < 7 {
                        [0, 5, 9, 7, 11, 13, 15][c as usize]
                    } else {
                        [16, 21, 25, 23, 27, 29, 31][(c - 7) as usize]
                    }
                })
                .unwrap_or(-1);
            if action < cfg.reveal_actions_count {
                frames.push(json!({
                    "k": "mv", "cmd": 1, "seq": step + 1,
                    "from": { "x": fx, "y": fy }, "to": { "x": fx, "y": fy },
                    "open": tile_id, "eat": -1, "cells": cells,
                }));
            } else {
                let (tx, ty) = sq_to_client(to);
                frames.push(json!({
                    "k": "mv", "cmd": 4, "seq": step + 1,
                    "from": { "x": fx, "y": fy }, "to": { "x": tx, "y": ty },
                    "open": if is_chance { tile_id } else { -1 },
                    "eat": -1, "cells": cells,
                }));
            }
            let (terminated, truncated, _) = env.check_game_over_conditions();
            if terminated || truncated {
                break;
            }
        }
        let truth = env.to_snapshot();
        let target = PositionSnapshot::from_fen(&truth.to_fen()).expect("fen 往返");
        let replayed = match replay_frames(&target, &frames) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "回放失败: {e}\nframes = {}",
                    serde_json::to_string(&frames).unwrap()
                );
                panic!("回放应成功");
            }
        };
        assert_eq!(replayed.slots, truth.slots);
        assert_eq!(replayed.current_player, truth.current_player);
        assert_eq!(
            replayed.chase_moves, truth.chase_moves,
            "chase 窗口应被重建"
        );
    }

    #[test]
    fn replay_rejects_corrupted_history() {
        let initial = all_hidden_snapshot(Variant::DarkChess4x8);
        let mut env = DarkChessEnv::from_snapshot(&initial).unwrap();
        env.step(
            action_lookup_tables(&env.config)
                .single_action((3 - 3) * 8 + 0)
                .unwrap(),
            Some(Piece::new(PieceType::General, Player::Black)),
        )
        .unwrap();
        let truth = env.to_snapshot();
        let target = PositionSnapshot::from_fen(&truth.to_fen()).unwrap();

        let mut cells = vec![30i64; 32];
        cells[3] = 11;
        let frames = vec![
            json!({ "k": "begin", "cmd": 63, "cells": vec![30; 32] }),
            json!({
                "k": "mv", "cmd": 1, "seq": 1,
                "from": { "x": 3, "y": 0 }, "to": { "x": 3, "y": 0 },
                "open": 27, "eat": -1, "cells": cells,
            }),
        ];
        assert!(
            replay_frames(&target, &frames).is_err(),
            "棋子码被篡改应校验失败"
        );
    }

    #[test]
    fn replay_rejects_missing_anchor() {
        let initial = all_hidden_snapshot(Variant::DarkChess4x8);
        let target = PositionSnapshot::from_fen(&initial.to_fen()).unwrap();
        assert!(replay_frames(&target, &[]).is_err());
        assert!(replay_frames(&target, &[json!({ "k": "ctx" })]).is_err());
    }
}
