use std::io;

use crate::i18n::tr;

#[derive(Debug)]
pub enum Error {
    Connect {
        host: String,
        port: u16,
        source: io::Error,
    },
    Ended,
    Timeout,
    Cancelled,
    Device { code: i64, message: String },
    Protocol(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect { host, port, source } => write!(
                f,
                "{}",
                trf!("не удалось подключиться к {host}:{port}: {source}", host = host, port = port, source = source)
            ),
            Error::Ended => f.write_str(tr("соединение закрыто")),
            Error::Timeout => f.write_str(tr("таймаут ожидания данных")),
            Error::Cancelled => f.write_str(tr("скачивание отменено")),
            Error::Device { code, message } => write!(
                f,
                "{}",
                trf!("регистратор вернул {code}: {message}", code = code, message = message)
            ),
            Error::Protocol(text) => write!(f, "{}", trf!("ошибка протокола: {text}", text = text)),
            Error::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Connect { source, .. } | Error::Io(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

pub fn device_message(code: i64) -> &'static str {
    match code {
        100 => tr("ОК"),
        101 => tr("неизвестная ошибка"),
        102 => tr("версия протокола не поддерживается"),
        103 => tr("запрос запрещён"),
        104 => tr("пользователь уже вошёл"),
        105 => tr("сессия не открыта"),
        106 => tr("неверный логин или пароль"),
        107 => tr("недостаточно прав"),
        119 => tr("нет данных за этот период"),
        203 => tr("неверный пароль"),
        205 => tr("пользователь не найден или адрес заблокирован"),
        207 => tr("адрес в чёрном списке"),
        _ => tr("ошибка регистратора"),
    }
}
