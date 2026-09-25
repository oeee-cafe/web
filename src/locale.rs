use std::collections::HashMap;

use fluent::FluentResource;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref LOCALES: HashMap<String, FluentResource> = {
        let mut locales = HashMap::new();
        locales.insert(
            "ko".to_string(),
            FluentResource::try_new(include_str!("../locales/ko.ftl").to_string())
                .expect("Korean locale file must be valid"),
        );
        locales.insert(
            "ja".to_string(),
            FluentResource::try_new(include_str!("../locales/ja.ftl").to_string())
                .expect("Japanese locale file must be valid"),
        );
        locales.insert(
            "en".to_string(),
            FluentResource::try_new(include_str!("../locales/en.ftl").to_string())
                .expect("English locale file must be valid"),
        );
        locales.insert(
            "zh".to_string(),
            FluentResource::try_new(include_str!("../locales/zh.ftl").to_string())
                .expect("Chinese locale file must be valid"),
        );
        locales
    };
}

#[cfg(test)]
mod tests {
    use super::LOCALES;
    use fluent::concurrent::FluentBundle;
    use fluent::FluentArgs;

    fn format(lang: &str, id: &str, args: &FluentArgs) -> String {
        let mut bundle = FluentBundle::new_concurrent(vec![lang.parse().unwrap()]);
        bundle.set_use_isolating(false);
        bundle.add_resource(&LOCALES[lang]).unwrap();
        let message = bundle
            .get_message(id)
            .unwrap_or_else(|| panic!("{lang} lacks {id}"));
        let mut errors = vec![];
        let formatted = bundle
            .format_pattern(message.value().unwrap(), Some(args), &mut errors)
            .to_string();
        assert!(errors.is_empty(), "{lang} {id}: {errors:?}");
        formatted
    }

    /// The profile hands over the month as a number; English names it, and
    /// has to name every one rather than falling through to December.
    #[test]
    fn member_since_names_the_month_in_every_language() {
        let mut args = FluentArgs::new();
        args.set("year", 2024);
        args.set("month", 3);
        assert_eq!(
            format("en", "profile-member-since", &args),
            "Member since March 2024"
        );
        assert_eq!(
            format("ko", "profile-member-since", &args),
            "2024년 3월 가입"
        );
        assert_eq!(
            format("ja", "profile-member-since", &args),
            "2024年3月から参加"
        );
        assert_eq!(format("zh", "profile-member-since", &args), "2024年3月加入");
        args.set("month", 1);
        assert_eq!(
            format("en", "profile-member-since", &args),
            "Member since January 2024"
        );
    }
}
