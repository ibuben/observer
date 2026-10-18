use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("не удалось подключиться к {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: io::Error,
    },
    #[error("соединение закрыто")]
    Ended,
    #[error("таймаут ожидания данных")]
    Timeout,
    #[error("скачивание отменено")]
    Cancelled,
    #[error("регистратор вернул {code}: {message}")]
    Device { code: i64, message: String },
    #[error("ошибка протокола: {0}")]
    Protocol(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

pub fn device_message(code: i64) -> &'static str {
    match code {
        100 => "ОК",
        101 => "неизвестная ошибка",
        102 => "версия протокола не поддерживается",
        103 => "запрос запрещён",
        104 => "пользователь уже вошёл",
        105 => "сессия не открыта",
        106 => "неверный логин или пароль",
        107 => "недостаточно прав",
        119 => "нет данных за этот период",
        203 => "неверный пароль",
        205 => "пользователь не найден или адрес заблокирован",
        207 => "адрес в чёрном списке",
        _ => "ошибка регистратора",
    }
}
