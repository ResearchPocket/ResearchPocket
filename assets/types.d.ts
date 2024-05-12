interface item_tag {
  tags: string[];
  id: number;
  uri: string;
  title: string;
  excerpt: string;
  time_added: number;
  favorite: boolean;
  lang: string;
}


declare let item_tags: item_tag[];
