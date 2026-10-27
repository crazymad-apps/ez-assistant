# 只改标准路径片段；已转义的 %%、主题函数和其他提示符片段原样保留。
__ez_terminal_prompt() {
    local previous=$?
    if [ -n "${EZ_ASSISTANT_TOOL_BIN-}" ]; then
        case ":$PATH:" in
            *":$EZ_ASSISTANT_TOOL_BIN:"*) ;;
            *) export PATH="$EZ_ASSISTANT_TOOL_BIN${PATH:+:$PATH}" ;;
        esac
    fi
    [[ -o promptpercent ]] || return $previous
    emulate -L zsh
    local text=$PROMPT output='' char next digits
    local -i index=1 braces=0
    while (( index <= ${#text} )); do
        char=${text[index]}
        (( index++ ))
        if [[ $char == '%' && index -le ${#text} ]]; then
            next=${text[index]}
            (( index++ ))
            digits=''
            while [[ $next == [0-9] && index -le ${#text} ]]; do
                digits+=$next
                next=${text[index]}
                (( index++ ))
            done
            # 日期/颜色参数和零宽度转义块中的 %d 等不是路径，不能改写。
            if [[ $next == (D|F|K) && ${text[index]} == '{' ]]; then
                output+=$char$digits$next
                braces=0
                while (( index <= ${#text} )); do
                    char=${text[index]}
                    output+=$char
                    (( index++ ))
                    [[ $char == '{' ]] && (( braces++ ))
                    [[ $char == '}' ]] && (( braces-- ))
                    (( braces == 0 )) && break
                done
                continue
            elif [[ $next == '{' ]]; then
                output+='%{'
                while (( index <= ${#text} )); do
                    char=${text[index]}
                    (( index++ ))
                    output+=$char
                    if [[ $char == '%' && ${text[index]} == '}' ]]; then
                        output+='}'
                        (( index++ ))
                        break
                    fi
                done
                continue
            fi
            if [[ $output == *'%32<...<' && $next == ('~'|/|d) ]]; then
                output+=$char$digits$next
                continue
            fi
            case $next in
                '~') output+='%32<...<%1~%<<' ;;
                /|d) output+='%32<...<%1/%<<' ;;
                *) output+=$char$digits$next ;;
            esac
        else
            output+=$char
        fi
    done
    PROMPT=$output
    return $previous
}
typeset -ga precmd_functions
precmd_functions+=(__ez_terminal_prompt)
