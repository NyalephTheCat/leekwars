import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_7309 extends AI {
public AI_7309() throws LeekRunException {
super(1, 3);
}
public void staticInit() throws LeekRunException {
}
private Object f_f(LegacyArrayLeekValue p_t) throws LeekRunException {final var u_t = new Box<LegacyArrayLeekValue>(AI_7309.this, p_t);
ops(1);ops(u_t.set(Array_arraySort_af(u_t.get(), new FunctionLeekValue(2) {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {var u_c1 = (values.length > 0 ?  values[0] : null);var u_c2 = (values.length > 1 ?  values[1] : null);
ops(1);Object u_x = ops(u_t.get(), 1);
ops(1); return (Object) sub(u_c1, u_c2);
}})), 1);
return u_t.get();
}
public Object runIA(Session session) throws LeekRunException {
ops(6); return f_f(new LegacyArrayLeekValue(AI_7309.this, new Object[] { 3l, 1l, 2l }, false));
}
protected String getAIString() { return "<snippet 7309>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 7309>", };}

protected int[] getErrorFilesID() { return new int[] {7309, };}

private LegacyArrayLeekValue Array_arraySort_af(Object a0, Object a1) throws LeekRunException {
LegacyArrayLeekValue x0; try { x0 = toLegacyArray(1, a0); } catch (ClassCastException e) { return new LegacyArrayLeekValue(AI_7309.this); }
FunctionLeekValue<Number> x1; try { x1 = toFunction(2, a1); } catch (ClassCastException e) { return new LegacyArrayLeekValue(AI_7309.this); }
return x0.arraySort_v1_3(this, x1);
}

}
